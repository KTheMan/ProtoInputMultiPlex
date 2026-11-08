//! Desktop mode arbitration. No Windows handles are persisted or OS input generated here.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Switched,
    #[default]
    Concurrent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopRouting {
    pub mode: Mode,
    pub multi_keyboard: bool,
    /// Empty means all eligible desktop applications. A nonempty list limits the scope.
    pub allowed_pids: Vec<u32>,
}
impl Default for DesktopRouting {
    fn default() -> Self {
        Self {
            mode: Mode::Concurrent,
            multi_keyboard: true,
            allowed_pids: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub hwnd: usize,
    pub pid: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    Granted { evicted: Option<usize> },
    Busy(usize),
}
#[derive(Debug)]
pub struct Policy {
    pub mode: Mode,
    pub root: Option<usize>,
    pub active: Option<usize>,
    owners: BTreeMap<u32, usize>,
    pub focus: Vec<Option<Window>>,
    active_users: Vec<bool>,
}
impl Policy {
    pub fn new(users: usize, mode: Mode) -> Self {
        Self {
            mode,
            root: None,
            active: None,
            owners: BTreeMap::new(),
            focus: vec![None; users],
            active_users: vec![true; users],
        }
    }
    /// Appends a new runtime user without changing any existing user index.
    pub fn append_user(&mut self) -> usize {
        let index = self.focus.len();
        self.focus.push(None);
        self.active_users.push(true);
        index
    }
    pub fn is_active(&self, user: usize) -> bool {
        self.active_users.get(user).copied().unwrap_or(false)
    }
    /// Retires a user slot permanently while preserving all other ownership.
    pub fn deactivate_user(&mut self, user: usize) -> bool {
        if !self.is_active(user) {
            return false;
        }
        self.active_users[user] = false;
        self.focus[user] = None;
        self.owners.retain(|_, owner| *owner != user);
        if self.root == Some(user) {
            self.root = None;
        }
        if self.active == Some(user) {
            self.active = None;
        }
        true
    }
    pub fn owner(&self, pid: u32) -> Option<usize> {
        self.owners.get(&pid).copied()
    }
    /// Initializes an appended seat's focus without changing the user's current active/root
    /// arbitration state or evicting an existing process owner.
    pub fn set_initial_focus(&mut self, user: usize, window: Window) -> bool {
        if !self.is_active(user)
            || self
                .owners
                .get(&window.pid)
                .is_some_and(|owner| *owner != user)
        {
            return false;
        }
        self.owners.insert(window.pid, user);
        self.focus[user] = Some(window);
        true
    }
    /// Read-only direct selection never evicts peers or releases sticky owners.
    pub fn can_select(&self, user: usize, window: Window) -> bool {
        self.is_active(user)
            && self.owner(window.pid).is_none_or(|owner| owner == user)
            && !(self.mode == Mode::Switched && self.root.is_some_and(|root| root != user))
    }
    pub fn select(&mut self, user: usize, window: Window) -> bool {
        if !self.can_select(user, window) {
            return false;
        }
        self.owners.insert(window.pid, user);
        self.focus[user] = Some(window);
        self.active = Some(user);
        true
    }
    pub fn claim(
        &mut self,
        user: usize,
        window: Window,
        held: &[bool],
        client_click: bool,
    ) -> Claim {
        if !self.is_active(user) {
            return Claim::Busy(user);
        }
        if client_click
            && self.mode == Mode::Switched
            && let Some(root) = self.root
            && root != user
        {
            return Claim::Busy(root);
        }
        let evicted = self
            .owners
            .get(&window.pid)
            .copied()
            .filter(|owner| *owner != user);
        if let Some(owner) = evicted
            && held[owner]
        {
            return Claim::Busy(owner);
        }
        if let Some(owner) = evicted
            && self.focus[owner].is_some_and(|w| w.pid == window.pid)
        {
            self.focus[owner] = None;
        }
        self.owners.insert(window.pid, user);
        self.focus[user] = Some(window);
        self.active = Some(user);
        Claim::Granted { evicted }
    }
    pub fn press(&mut self, user: usize) {
        if self.is_active(user) && self.mode == Mode::Switched {
            self.root = Some(user);
        }
    }
    pub fn release(&mut self, user: usize) {
        if self.root == Some(user) {
            self.root = None;
        }
    }
    pub fn mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.root = None;
    }
    pub fn forget(&mut self, pid: u32) {
        self.owners.remove(&pid);
        for focus in &mut self.focus {
            if focus.is_some_and(|w| w.pid == pid) {
                *focus = None;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn w(pid: u32) -> Window {
        Window {
            pid,
            hwnd: pid as usize + 100,
        }
    }
    #[test]
    fn switched_root_blocks_clicks_but_not_other_window_actions() {
        let mut p = Policy::new(2, Mode::Switched);
        assert_eq!(
            p.claim(0, w(10), &[false, false], true),
            Claim::Granted { evicted: None }
        );
        p.press(0);
        assert_eq!(p.claim(1, w(20), &[true, false], true), Claim::Busy(0));
        assert_eq!(
            p.claim(1, w(20), &[true, false], false),
            Claim::Granted { evicted: None }
        );
        p.release(0);
        assert!(matches!(
            p.claim(1, w(20), &[false, false], true),
            Claim::Granted { .. }
        ));
    }
    #[test]
    fn concurrent_process_ownership_and_keyboard_focus_survive_other_users() {
        let mut p = Policy::new(2, Mode::Concurrent);
        p.claim(0, w(10), &[false, false], true);
        p.claim(1, w(20), &[true, false], true);
        assert_eq!(p.focus, [Some(w(10)), Some(w(20))]);
        assert_eq!(p.claim(1, w(10), &[true, true], true), Claim::Busy(0));
        assert_eq!(
            p.claim(1, w(10), &[false, true], true),
            Claim::Granted { evicted: Some(0) }
        );
        assert_eq!(p.focus, [None, Some(w(10))]);
        p.forget(10);
        assert_eq!(p.focus, [None, None]);
    }
    #[test]
    fn changing_mode_clears_root_without_losing_focus() {
        let mut p = Policy::new(1, Mode::Switched);
        p.claim(0, w(10), &[false], true);
        p.press(0);
        p.mode(Mode::Concurrent);
        assert_eq!(p.root, None);
        assert_eq!(p.focus, [Some(w(10))]);
    }

    #[test]
    fn append_and_retire_keep_existing_indices_and_ownership() {
        let mut p = Policy::new(2, Mode::Concurrent);
        p.claim(0, w(10), &[false, false], true);
        p.claim(1, w(20), &[false, false], true);
        let clone = p.append_user();
        assert_eq!(clone, 2);
        assert_eq!(p.focus, [Some(w(10)), Some(w(20)), None]);
        assert!(p.is_active(clone));
        p.claim(clone, w(30), &[false, false, false], true);
        assert!(p.deactivate_user(clone));
        assert_eq!(p.focus, [Some(w(10)), Some(w(20)), None]);
        assert_eq!(p.active, None);
        assert!(!p.is_active(clone));
        assert_eq!(
            p.claim(clone, w(40), &[false, false, false], true),
            Claim::Busy(clone)
        );
        assert_eq!(
            p.claim(1, w(20), &[false, false, false], true),
            Claim::Granted { evicted: None }
        );
        assert!(!p.deactivate_user(clone));
    }

    #[test]
    fn retiring_owner_releases_only_its_process_lease() {
        let mut p = Policy::new(2, Mode::Concurrent);
        p.claim(0, w(10), &[false, false], true);
        p.claim(0, w(11), &[false, false], true);
        p.claim(1, w(20), &[false, false], true);
        p.deactivate_user(0);
        assert_eq!(p.focus, [None, Some(w(20))]);
        assert_eq!(
            p.claim(1, w(10), &[false, false], true),
            Claim::Granted { evicted: None }
        );
        assert_eq!(
            p.claim(1, w(11), &[false, false], true),
            Claim::Granted { evicted: None }
        );
        assert_eq!(p.focus, [None, Some(w(11))]);
    }
}

#[cfg(test)]
mod explicit_selection_tests {
    use super::*;
    #[test]
    fn non_evicting_selection_keeps_sticky_owners_and_rejects_busy_atomically() {
        let mut policy = Policy::new(2, Mode::Concurrent);
        let first = Window { pid: 11, hwnd: 101 };
        let next = Window { pid: 12, hwnd: 102 };
        assert!(policy.select(0, first));
        assert!(policy.select(0, next));
        assert_eq!(policy.owner(11), Some(0));
        let before = format!("{policy:?}");
        assert!(!policy.select(1, first));
        assert_eq!(format!("{policy:?}"), before);
        policy.mode(Mode::Switched);
        policy.root = Some(0);
        let before = format!("{policy:?}");
        assert!(!policy.select(1, Window { pid: 13, hwnd: 103 }));
        assert_eq!(format!("{policy:?}"), before);
        policy.forget(11);
        assert_eq!(policy.owner(11), None);
    }
}
