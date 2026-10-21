// Действия UI: клавиатура, мышь и вставка сводятся к одному списку.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Side,
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Pane(Pane),
    Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Help,
    Profile,
    HangUp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    FileDecision(bool),
    PageUp,
    PageDown,
    Confirm,
    Cancel,
    NextPane,
    NextLink(bool),
    OpenLink(usize),
    SendFile,
    CancelFile,
    Update,
    EditAvatar,
    ShowProfile(usize),
    CycleAvatars,
    FocusInput,
    Scroll(Pane, i16),
    Insert(char),
    Backspace,
    Paste(String),
    Copy(String),
    Host,
    Connect,
    OpenSettings,
    Quit,
    ToggleMute,
    ToggleDeaf,
    ToggleAutoGain,
    ToggleEcho,
    GainPeer(i16),
    GainMic(i16),
    DeviceCycle { input: bool },
    Accept,
    Decline,
    HangUp,
    Help,
    Ptt(bool),
    Click(Pane, usize),
}
