use crossterm::event::KeyEvent;

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    /// (cols, rows) as crossterm reports them.
    Resize(u16, u16),
    PtyOutput { id: usize, bytes: Vec<u8> },
    PtyExit { id: usize },
    Tick,
}
