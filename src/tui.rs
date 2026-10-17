mod command;
mod render;
mod state;
#[cfg(test)]
mod tests;
mod worker;

use crate::{config::Config, engine::Engine};
use anyhow::{Result, bail};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use state::App;
use std::{
    io::{self, IsTerminal},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use worker::View;

struct Screen;
impl Screen {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(guard)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

pub fn run(config: Config, stop: Arc<AtomicBool>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("TUI 需要交互终端");
    }
    let engine = Engine::new(config, stop.clone())?;
    let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let (sender, receiver) = mpsc::sync_channel(16);
    let worker_view = view.clone();
    let worker_stop = stop.clone();
    let handle = thread::spawn(move || worker::run(engine, receiver, worker_view, worker_stop));
    let result = (|| -> Result<()> {
        let _screen = Screen::enter()?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        terminal.clear()?;
        let mut app = App::default();
        let mut revision = 0;
        let mut dirty = true;
        while !stop.load(Ordering::Relaxed) && !app.quit {
            let snapshot = view.lock().unwrap().clone();
            app.reconcile(&snapshot);
            if dirty || revision != snapshot.revision {
                terminal.draw(|frame| render::draw(frame, &mut app, &snapshot))?;
                revision = snapshot.revision;
                dirty = false;
            }
            if app.search_due.is_some_and(|due| Instant::now() >= due) {
                app.flush_search(&sender);
                dirty = true;
            }
            if handle.is_finished() {
                break;
            }
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            match event::read()? {
                Event::Resize(_, _) => dirty = true,
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    app.key(key, &snapshot, &sender);
                    dirty = true;
                }
                Event::Mouse(mouse) => {
                    app.mouse(mouse, &snapshot, &sender);
                    dirty = true;
                }
                _ => {}
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    drop(sender);
    let worker_result = handle
        .join()
        .map_err(|_| anyhow::anyhow!("界面工作线程异常退出"))?;
    result?;
    worker_result
}
