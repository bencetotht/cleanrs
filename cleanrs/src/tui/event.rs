//! Messages cross thread boundaries; App owns all presentation state.
use crossterm::event::KeyEvent;

use super::worker::WorkerEvent;

pub enum Event {
    Key(KeyEvent),
    Resize,
    Tick,
    Worker(WorkerEvent),
    InputError(String),
}
