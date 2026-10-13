//! One binding table drives dispatch, help, and footer hints.
use super::app::{Action, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub struct Binding {
    pub keys: &'static [KeyCode],
    pub modifiers: KeyModifiers,
    pub label: &'static str,
    pub help: &'static str,
    pub footer: Option<&'static str>,
    pub global: bool,
    pub action: Action,
}

macro_rules! binding {
    ($keys:expr, $mods:expr, $label:expr, $help:expr, $footer:expr, $global:expr, $action:expr) => {
        Binding {
            keys: $keys,
            modifiers: $mods,
            label: $label,
            help: $help,
            footer: $footer,
            global: $global,
            action: $action,
        }
    };
}
use KeyCode::*;
use KeyModifiers as M;
pub static BINDINGS: &[Binding] = &[
    binding!(
        &[Char('j'), Down],
        M::NONE,
        "j / ↓",
        "Next row",
        Some("j/k move"),
        false,
        Action::Move(1)
    ),
    binding!(
        &[Char('k'), Up],
        M::NONE,
        "k / ↑",
        "Previous row",
        None,
        false,
        Action::Move(-1)
    ),
    binding!(
        &[Char('g')],
        M::NONE,
        "gg",
        "First row",
        None,
        false,
        Action::First
    ),
    binding!(
        &[Char('G')],
        M::NONE,
        "G",
        "Last row",
        None,
        false,
        Action::Last
    ),
    binding!(
        &[Char('d')],
        M::CONTROL,
        "Ctrl-d",
        "Half page down",
        None,
        false,
        Action::HalfPage(1)
    ),
    binding!(
        &[Char('u')],
        M::CONTROL,
        "Ctrl-u",
        "Half page up",
        None,
        false,
        Action::HalfPage(-1)
    ),
    binding!(
        &[Char('l'), Enter, Right],
        M::NONE,
        "l / Enter / →",
        "Open directory",
        Some("l open"),
        false,
        Action::Open
    ),
    binding!(
        &[Char('h'), Left, Backspace],
        M::NONE,
        "h / ← / Backspace",
        "Parent directory; reuse visited sizes",
        Some("h up"),
        false,
        Action::Parent
    ),
    binding!(
        &[Char('-')],
        M::NONE,
        "-",
        "Previous directory",
        None,
        false,
        Action::Previous
    ),
    binding!(
        &[Char('/')],
        M::NONE,
        "/",
        "Filter; Enter confirms, Esc clears",
        Some("/ filter"),
        false,
        Action::StartFilter
    ),
    binding!(
        &[Char('n')],
        M::NONE,
        "n",
        "Next filtered match",
        None,
        false,
        Action::Match(1)
    ),
    binding!(
        &[Char('N')],
        M::NONE,
        "N",
        "Previous filtered match",
        None,
        false,
        Action::Match(-1)
    ),
    binding!(
        &[Char('s')],
        M::NONE,
        "s",
        "Sort: size → name → age",
        Some("s sort"),
        false,
        Action::Sort
    ),
    binding!(
        &[Char('i')],
        M::NONE,
        "i",
        "Toggle accounting info",
        Some("i info"),
        false,
        Action::Info
    ),
    binding!(
        &[Char('r')],
        M::NONE,
        "r",
        "Cancel and rescan current subtree",
        None,
        false,
        Action::Rescan
    ),
    binding!(
        &[Char('T')],
        M::NONE,
        "T",
        "Targets for highlighted directory or current folder",
        Some("T targets"),
        false,
        Action::Targets
    ),
    binding!(
        &[Char(' ')],
        M::NONE,
        "Space",
        "Toggle item and move down; apply visual range",
        Some("space toggle"),
        false,
        Action::Toggle
    ),
    binding!(
        &[Char('v')],
        M::NONE,
        "v",
        "Start/apply visual range; Esc cancels",
        Some("v range"),
        false,
        Action::Visual
    ),
    binding!(
        &[Char('a')],
        M::CONTROL,
        "Ctrl-a",
        "Select all eligible items",
        None,
        false,
        Action::SelectAll
    ),
    binding!(
        &[Char('r')],
        M::CONTROL,
        "Ctrl-r",
        "Invert tickable items",
        None,
        false,
        Action::Invert
    ),
    binding!(
        &[Char('u')],
        M::NONE,
        "u",
        "Clear all ticks",
        None,
        false,
        Action::ClearSelection
    ),
    binding!(
        &[Char('1')],
        M::NONE,
        "1",
        "Safe preset; discard manual ticks",
        Some("1/2/3 level"),
        false,
        Action::Preset(cleanrs_core::targets::CleanupLevel::Safe)
    ),
    binding!(
        &[Char('2')],
        M::NONE,
        "2",
        "Standard preset; discard manual ticks",
        None,
        false,
        Action::Preset(cleanrs_core::targets::CleanupLevel::Standard)
    ),
    binding!(
        &[Char('3')],
        M::NONE,
        "3",
        "Aggressive preset; discard manual ticks",
        None,
        false,
        Action::Preset(cleanrs_core::targets::CleanupLevel::Aggressive)
    ),
    binding!(
        &[Char('m')],
        M::NONE,
        "m",
        "Toggle machine-wide providers and re-detect",
        Some("m scope"),
        false,
        Action::Machine
    ),
    binding!(
        &[Char('x')],
        M::NONE,
        "x",
        "Rebuild selected plan and review",
        Some("x review"),
        false,
        Action::Review
    ),
    binding!(
        &[Char('y')],
        M::NONE,
        "y",
        "Confirm permanent cleanup / stop after current action",
        Some("y confirm"),
        false,
        Action::Confirm
    ),
    binding!(
        &[Char('n')],
        M::NONE,
        "n",
        "Cancel review / stop request",
        Some("n cancel"),
        false,
        Action::Cancel
    ),
    binding!(
        &[Char('[')],
        M::NONE,
        "[",
        "Scroll provider output up",
        Some("[/] output"),
        false,
        Action::OutputScroll(1)
    ),
    binding!(
        &[Char(']')],
        M::NONE,
        "]",
        "Scroll provider output down",
        None,
        false,
        Action::OutputScroll(-1)
    ),
    binding!(
        &[Char('?')],
        M::NONE,
        "?",
        "Toggle help",
        Some("? help"),
        true,
        Action::Help
    ),
    binding!(
        &[Char('q'), Esc],
        M::NONE,
        "q / Esc",
        "Back / close help; confirm quit from browser",
        Some("q quit"),
        true,
        Action::Back
    ),
    binding!(
        &[Char('c')],
        M::CONTROL,
        "Ctrl-c",
        "Confirm quit",
        None,
        true,
        Action::Quit
    ),
];

pub fn bindings(screen: Screen) -> impl Iterator<Item = &'static Binding> {
    BINDINGS
        .iter()
        .filter(move |binding| binding.global || applies(&binding.action, screen))
}
fn applies(action: &Action, screen: Screen) -> bool {
    match action {
        Action::Move(_) | Action::First | Action::Last | Action::HalfPage(_) => true,
        Action::StartFilter | Action::Match(_) | Action::Sort | Action::Rescan => {
            matches!(screen, Screen::Browser | Screen::Targets)
        }
        Action::Open | Action::Parent | Action::Previous | Action::Info | Action::Targets => {
            screen == Screen::Browser
        }
        Action::Toggle
        | Action::Visual
        | Action::SelectAll
        | Action::Invert
        | Action::ClearSelection
        | Action::Preset(_)
        | Action::Machine
        | Action::Review => screen == Screen::Targets,
        Action::Confirm | Action::Cancel => matches!(screen, Screen::Review | Screen::Run),
        Action::OutputScroll(_) => screen == Screen::Run,
        _ => false,
    }
}
pub fn footer_for(screen: Screen, width: usize) -> String {
    let mut hints: Vec<_> = bindings(screen)
        .filter_map(|binding| binding.footer.map(|hint| (binding, hint)))
        .collect();
    let priority = |action: &Action| match action {
        Action::Back | Action::Help | Action::Confirm | Action::Cancel => 0,
        Action::Targets | Action::Toggle | Action::Review => 1,
        Action::Move(_) | Action::Open | Action::Parent | Action::Preset(_) => 2,
        _ => 3,
    };
    loop {
        let text = hints
            .iter()
            .map(|(b, hint)| match b.action {
                Action::Back if screen == Screen::Run => "q stop",
                Action::Back if screen != Screen::Browser => "q back",
                _ => hint,
            })
            .collect::<Vec<_>>()
            .join("  ");
        if ratatui::text::Line::raw(&text).width() <= width || hints.is_empty() {
            return text;
        }
        let drop = hints
            .iter()
            .enumerate()
            .max_by_key(|(i, (b, _))| (priority(&b.action), *i))
            .unwrap()
            .0;
        hints.remove(drop);
    }
}
/// Context-sensitive wording mirrors the actual handler, including stop safety.
pub fn help_for(binding: &Binding, screen: Screen) -> &str {
    match binding.action {
        Action::Rescan if screen == Screen::Targets => "Re-detect targets; retain surviving ticks",
        Action::Targets => "Targets; target row uses its containing folder",
        Action::Confirm if screen == Screen::Review => "Confirm permanent cleanup (Enter is inert)",
        Action::Confirm if screen == Screen::Run => "Confirm stop after the current action",
        Action::Cancel if screen == Screen::Review => "Cancel review; retain ticks",
        Action::Cancel if screen == Screen::Run => "Cancel stop request",
        Action::Back | Action::Quit if screen == Screen::Run => {
            "Ask to stop after the current action"
        }
        Action::Back if screen != Screen::Browser => "Back one screen / close help",
        _ => binding.help,
    }
}

#[derive(Default)]
pub struct Keymap {
    pending_g: bool,
}
impl Keymap {
    /// A quit prompt never forwards navigation/cleanup keys to the underlying view.
    pub fn quit_action(&mut self, key: KeyEvent) -> Option<Action> {
        self.pending_g = false;
        match key.code {
            Char('y') if (key.modifiers - M::SHIFT).is_empty() => Some(Action::Confirm),
            Char('n' | 'q') | Esc | Enter => Some(Action::Cancel),
            Char('c') if key.modifiers == M::CONTROL => Some(Action::Cancel),
            _ => None,
        }
    }
    pub fn action(&mut self, key: KeyEvent, screen: Screen, filtering: bool) -> Option<Action> {
        let modifiers = key.modifiers - M::SHIFT;
        if filtering && !(key.code == Char('c') && modifiers == M::CONTROL) {
            self.pending_g = false;
            return match key.code {
                Enter => Some(Action::ConfirmFilter),
                Esc => Some(Action::ClearFilter),
                Backspace => Some(Action::FilterBackspace),
                Char(c) if modifiers.is_empty() => Some(Action::FilterChar(c)),
                _ => None,
            };
        }
        let binding = bindings(screen)
            .find(|binding| binding.keys.contains(&key.code) && binding.modifiers == modifiers);
        let is_g = binding.is_some_and(|b| b.label == "gg");
        let complete_g = is_g && self.pending_g;
        self.pending_g = is_g && !complete_g;
        if is_g && !complete_g {
            return None;
        }
        binding.map(|binding| binding.action.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_dispatches_and_gg_requires_consecutive_keys() {
        let mut map = Keymap::default();
        for binding in BINDINGS.iter().filter(|b| b.label != "gg") {
            let key = KeyEvent::new(binding.keys[0], binding.modifiers);
            let screen = [
                Screen::Browser,
                Screen::Targets,
                Screen::Review,
                Screen::Run,
            ]
            .into_iter()
            .find(|s| binding.global || applies(&binding.action, *s))
            .unwrap();
            assert_eq!(map.action(key, screen, false), Some(binding.action.clone()));
        }
        let g = KeyEvent::new(Char('g'), M::NONE);
        assert_eq!(map.action(g, Screen::Browser, false), None);
        assert_eq!(map.action(g, Screen::Browser, false), Some(Action::First));
        assert_eq!(map.action(g, Screen::Browser, false), None);
        map.action(KeyEvent::new(Char('j'), M::NONE), Screen::Browser, false);
        assert_eq!(map.action(g, Screen::Browser, false), None);
        assert_eq!(
            map.action(KeyEvent::new(Char('q'), M::NONE), Screen::Browser, true),
            Some(Action::FilterChar('q'))
        );
    }
}
