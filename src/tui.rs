mod artwork;
mod capabilities;
mod command;
mod display;
mod help;
mod map;
mod output;
mod render;
mod state;
mod styles;
#[cfg(test)]
mod tests;
mod text;
mod worker;

use crate::{config::Config, engine::Engine};
use anyhow::{Result, bail};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind,
    },
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

/// Pointer motion, drags and releases never change visible state, so they must not repaint.
fn mouse_redraws(kind: event::MouseEventKind) -> bool {
    use event::MouseEventKind::{Drag, Moved, Up};
    !matches!(kind, Moved | Drag(_) | Up(_))
}

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
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

pub fn run(config: Config, stop: Arc<AtomicBool>) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(crate::t!("tui_requires_terminal"));
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
        let mut pending = std::collections::VecDeque::new();
        let mut artwork = artwork::Controller::new(artwork_config, theme, &mut pending);
        // Enabled only after startup probing, which reads raw terminal replies itself.
        execute!(io::stdout(), EnableBracketedPaste)?;
        let writer = output::TerminalWriter::new(io::stdout(), theme == crate::config::Theme::Mono);
        let mut terminal = Terminal::new(CrosstermBackend::new(writer))?;
        execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;
        app.mouse_enabled = mouse;
        app.jump_binding = jump_binding;
        if let Err(error) = app.editor.load_history(history_path) {
            app.message = crate::t!("history_load_failed", "detail" => error);
        }
        app.theme = theme;
        let mut revision = 0;
        let mut immediate_revision = 0;
        let mut last_draw = Instant::now();
        let mut dirty = true;
        while !stop.load(Ordering::Relaxed) && !app.quit {
            let size = terminal.size()?;
            app.update_size(size.width, size.height);
            let snapshot = view.lock().unwrap().snapshot();
            app.reconcile(&snapshot);
            dirty |= app.expire_message(Instant::now());
            if let Some(artwork) = &mut artwork {
                let size = terminal.size()?;
                let screen = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                view.lock().unwrap().artwork_station = artwork.demand(&app, &snapshot, screen);
                dirty |= artwork.update(&mut app, &snapshot, screen);
            }
            app.request_selected_backups(&snapshot, &sender);
            app.request_facets(&sender);
            dirty |= app.update_map(&mut view.lock().unwrap(), &snapshot, &sender);
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
            if !app.too_small && app.search_due.is_some_and(|due| Instant::now() >= due) {
                app.flush_search(&sender);
                dirty = true;
            }
            if handle.is_finished() {
                break;
            }
            let event = if let Some(event) = pending.pop_front() {
                event
            } else {
                if !event::poll(Duration::from_millis(100))? {
                    continue;
                }
                event::read()?
            };
            match event {
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
                Event::Paste(text) => {
                    app.paste(&text);
                    dirty = true;
                }
                Event::Mouse(mouse) => {
                    app.mouse(mouse, &snapshot, &sender);
                    dirty |= mouse_redraws(mouse.kind);
                }
                _ => {}
            }
        }
        drop(artwork);
        stop.store(true, Ordering::Relaxed);
        terminal.draw(|frame| {
            let hint = if matches!(
                view.lock().unwrap().status.recording_state,
                crate::engine::RecordingState::Probing
                    | crate::engine::RecordingState::Recording
                    | crate::engine::RecordingState::Finalizing
            ) {
                crate::t!("exiting_recording")
            } else {
                crate::t!("exiting_cleanup")
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
        .map_err(|_| anyhow::anyhow!(crate::t!("ui_worker_failed")))?;
    result?;
    worker_result
}
