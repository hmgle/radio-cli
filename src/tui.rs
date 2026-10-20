mod artwork;
mod command;
mod output;
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
    },
    thread,
    time::{Duration, Instant},
};
use worker::View;

struct Screen;
impl Screen {
    fn enter(mouse: bool) -> Result<Self> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        if mouse {
            execute!(io::stdout(), EnableMouseCapture)?;
        }
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
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|s| !s.is_empty());
    let theme = config.settings.theme.effective(no_color);
    let mouse = config.settings.mouse;
    let jump_binding = config.settings.keybindings.jump_to_playing.clone();
    let artwork_config = config.clone();
    let history_path = config.data_dir.join("command-history.json");
    if theme == crate::config::Theme::Mono {
        crossterm::style::force_color_output(false);
    }
    let engine = Engine::new(config, stop.clone())?;
    let mut app = App::restore(&engine.store, engine.config.settings.resume)?;
    let view = Arc::new(Mutex::new(View::new(engine.status.clone())));
    let (sender, receiver) = command::channel(16);
    let worker_view = view.clone();
    let worker_stop = stop.clone();
    let initial_filter = app.filter.clone();
    let handle = thread::spawn(move || {
        worker::run(engine, receiver, worker_view, worker_stop, initial_filter)
    });
    let result = (|| -> Result<()> {
        let _screen = Screen::enter(mouse)?;
        let mut artwork = artwork::Controller::new(artwork_config, theme);
        let writer = output::TerminalWriter::new(io::stdout(), theme == crate::config::Theme::Mono);
        let mut terminal = Terminal::new(CrosstermBackend::new(writer))?;
        execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;
        app.mouse_enabled = mouse;
        app.jump_binding = jump_binding;
        if let Err(error) = app.editor.load_history(history_path) {
            app.message = format!("历史加载失败: {error}");
        }
        app.theme = theme;
        let mut revision = 0;
        let mut immediate_revision = 0;
        let mut last_draw = Instant::now();
        let mut dirty = true;
        while !stop.load(Ordering::Relaxed) && !app.quit {
            let snapshot = view.lock().unwrap().snapshot();
            app.reconcile(&snapshot);
            if let Some(artwork) = &mut artwork {
                let size = terminal.size()?;
                dirty |= artwork.update(
                    &mut app,
                    &snapshot,
                    ratatui::layout::Rect::new(0, 0, size.width, size.height),
                );
            }
            app.request_selected_backups(&snapshot, &sender);
            app.request_facets(&sender);
            if dirty
                || revision != snapshot.revision
                    && (immediate_revision != snapshot.immediate_revision
                        || last_draw.elapsed() >= Duration::from_secs(1))
            {
                terminal.draw(|frame| render::draw(frame, &mut app, &snapshot))?;
                revision = snapshot.revision;
                immediate_revision = snapshot.immediate_revision;
                last_draw = Instant::now();
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
                Event::Resize(width, height) => {
                    app.update_size(width, height);
                    dirty = true;
                }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let size = terminal.size()?;
                    app.update_size(size.width, size.height);
                    let old_mouse = app.mouse_enabled;
                    app.key(key, &snapshot, &sender);
                    if app.mouse_enabled != old_mouse {
                        if app.mouse_enabled {
                            execute!(io::stdout(), EnableMouseCapture)?;
                        } else {
                            execute!(io::stdout(), DisableMouseCapture)?;
                        }
                    }
                    if let Some(value) = app.clipboard.take() {
                        use std::io::Write;
                        io::stdout().write_all(&output::clipboard_sequence(&value))?;
                        io::stdout().flush()?;
                    }
                    dirty = true;
                }
                Event::Mouse(mouse) => {
                    app.mouse(mouse, &snapshot, &sender);
                    dirty = true;
                }
                _ => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        terminal.draw(|frame| {
            let hint = if matches!(
                view.lock().unwrap().status.recording_state,
                crate::engine::RecordingState::Probing
                    | crate::engine::RecordingState::Recording
                    | crate::engine::RecordingState::Finalizing
            ) {
                "radio · 正在退出 · 正在收尾录音…"
            } else {
                "radio · 正在退出 · 清理媒体…"
            };
            frame.render_widget(ratatui::widgets::Paragraph::new(hint), frame.area());
        })?;
        // Keep the alternate screen and hint until bounded media cleanup ends.
        while !handle.is_finished() {
            thread::sleep(Duration::from_millis(20));
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
