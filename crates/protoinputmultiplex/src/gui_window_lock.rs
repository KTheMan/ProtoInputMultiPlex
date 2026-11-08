//! Transient pre-start window selections for per-user Window bounds.
//!
//! The selected HWND and process identity are intentionally never written to
//! Profile. The routing worker revalidates each selection after startup.
use anyhow::{Result, ensure};
use mux_core::{DeviceKind, Profile};
use std::collections::BTreeMap;

use crate::window_selection::Selection;

#[derive(Clone, Debug, Default)]
pub struct StagedWindows {
    entries: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    pub selection: Selection,
    pub title: String,
}

#[derive(Clone, Debug)]
struct Entry {
    stage: Stage,
    scope: Scope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    seat_id: String,
    mouse: String,
    keyboard: Option<String>,
    mode: mux_core::routing::Mode,
    multi_keyboard: bool,
    allowed_pids: Vec<u32>,
}

impl StagedWindows {
    /// Stage a current inventory selection for one eligible Window-bounds seat.
    pub fn stage(
        &mut self,
        profile: &Profile,
        user: usize,
        selection: Selection,
        title: String,
    ) -> Result<()> {
        self.prune(profile);
        let seat = profile
            .seats
            .get(user)
            .ok_or_else(|| anyhow::anyhow!("Unknown user index (indices start at zero)"))?;
        ensure!(
            selection.user == user,
            "window selection belongs to a different user"
        );
        ensure!(selection.pid != 0, "window selection requires a process ID");
        ensure!(
            selection.hwnd != 0,
            "window selection requires a window handle"
        );
        mux_windows::targets::parse_creation_token(&selection.creation_token)?;
        let scope = scope_for(profile, user)?;
        ensure!(
            scope.allowed_pids.is_empty() || scope.allowed_pids.contains(&selection.pid),
            "window is outside this profile's allowed application scope"
        );
        let seat_id = seat.identity();
        ensure!(
            !self.entries.iter().any(|(other_id, entry)| {
                other_id != &seat_id && entry.stage.selection.pid == selection.pid
            }),
            "the same application process cannot be staged for multiple users"
        );

        self.entries.insert(
            seat_id,
            Entry {
                stage: Stage {
                    selection,
                    title: bounded_title(title),
                },
                scope,
            },
        );
        Ok(())
    }

    /// Remove stages whose user, device, routing policy, or Window-bounds mode changed.
    pub fn prune(&mut self, profile: &Profile) {
        self.entries.retain(|seat_id, entry| {
            let Some(user) = profile
                .seats
                .iter()
                .position(|seat| seat.identity() == *seat_id)
            else {
                return false;
            };
            let Ok(scope) = scope_for(profile, user) else {
                return false;
            };
            scope == entry.scope
                && entry.stage.selection.pid != 0
                && entry.stage.selection.hwnd != 0
                && mux_windows::targets::parse_creation_token(&entry.stage.selection.creation_token)
                    .is_ok()
                && (scope.allowed_pids.is_empty()
                    || scope.allowed_pids.contains(&entry.stage.selection.pid))
        });
    }

    /// Read a staged selection only while its current profile scope still matches.
    pub fn get(&self, profile: &Profile, user: usize) -> Option<&Stage> {
        let seat = profile.seats.get(user)?;
        let entry = self.entries.get(&seat.identity())?;
        let scope = scope_for(profile, user).ok()?;
        (scope == entry.scope
            && (scope.allowed_pids.is_empty()
                || scope.allowed_pids.contains(&entry.stage.selection.pid)))
        .then_some(&entry.stage)
    }

    /// Drain valid selections in current profile order, updating their user indices.
    pub fn take_for_start(&mut self, profile: &Profile) -> Vec<Selection> {
        self.prune(profile);
        let entries = std::mem::take(&mut self.entries);
        profile
            .seats
            .iter()
            .enumerate()
            .filter_map(|(user, seat)| {
                let mut selection = entries.get(&seat.identity())?.stage.selection.clone();
                selection.user = user;
                Some(selection)
            })
            .collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Clear one user's staged selection; an invalid user index is a no-op.
    pub fn clear_user(&mut self, profile: &Profile, user: usize) {
        if let Some(seat) = profile.seats.get(user) {
            self.entries.remove(&seat.identity());
        }
    }
}

fn scope_for(profile: &Profile, user: usize) -> Result<Scope> {
    let seat = profile
        .seats
        .get(user)
        .ok_or_else(|| anyhow::anyhow!("Unknown user index (indices start at zero)"))?;
    ensure!(
        profile
            .seats
            .iter()
            .filter(|candidate| candidate.identity() == seat.identity())
            .count()
            == 1,
        "user identity is ambiguous in this profile"
    );
    let desktop = profile
        .desktop
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Window selection requires automatic desktop routing"))?;
    ensure!(
        desktop.multi_keyboard,
        "Enable independent keyboards before selecting a window"
    );
    ensure!(
        seat.cursor.bounds_mode == mux_core::desktop::BoundsMode::Window,
        "Staged window selection requires Window bounds"
    );
    let mouse = mux_core::emulated::parse(&seat.mouse)?
        .filter(|device| device.kind == DeviceKind::Mouse)
        .ok_or_else(|| anyhow::anyhow!("Window selection requires a virtual mouse"))?;
    if let Some(path) = seat.keyboard.as_deref() {
        let keyboard = mux_core::emulated::parse(path)?;
        ensure!(
            keyboard.is_some_and(|device| device.kind == DeviceKind::Keyboard),
            "Window selection requires a virtual keyboard when one is assigned"
        );
    }
    let mut allowed_pids = desktop.allowed_pids.clone();
    allowed_pids.sort_unstable();
    allowed_pids.dedup();
    Ok(Scope {
        seat_id: seat.identity(),
        mouse: mouse.path,
        keyboard: seat.keyboard.clone(),
        mode: desktop.mode,
        multi_keyboard: desktop.multi_keyboard,
        allowed_pids,
    })
}

fn bounded_title(title: String) -> String {
    title
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux_core::Seat;

    fn profile() -> Profile {
        let mut first = Seat {
            id: "seat-a".into(),
            name: "A".into(),
            mouse: "virtual://mouse/1".into(),
            keyboard: Some("virtual://keyboard/1".into()),
            target_pid: 0,
            desktop_start: None,
            cursor: Default::default(),
            annotation: Default::default(),
            magnifier: Default::default(),
            output: Default::default(),
        };
        first.cursor.bounds_mode = mux_core::desktop::BoundsMode::Window;
        let mut second = first.clone();
        second.id = "seat-b".into();
        second.name = "B".into();
        second.mouse = "virtual://mouse/2".into();
        second.keyboard = Some("virtual://keyboard/2".into());
        Profile {
            desktop: Some(mux_core::routing::DesktopRouting {
                allowed_pids: vec![41, 42, 41],
                ..Default::default()
            }),
            seats: vec![first, second],
            ..Default::default()
        }
    }

    fn selection(user: usize, pid: u32, hwnd: usize) -> Selection {
        Selection {
            request_id: String::new(),
            user,
            pid,
            hwnd,
            creation_token: "13245678901234567890".into(),
        }
    }

    #[test]
    fn stages_are_isolated_by_seat_and_remapped_after_reorder() {
        let mut profile = profile();
        let mut stages = StagedWindows::default();
        stages
            .stage(&profile, 0, selection(0, 41, 0x101), "Window A".into())
            .unwrap();
        stages
            .stage(&profile, 1, selection(1, 42, 0x202), "Window B".into())
            .unwrap();
        assert_eq!(stages.get(&profile, 0).unwrap().title, "Window A");
        assert_eq!(stages.get(&profile, 1).unwrap().title, "Window B");

        stages.clear_user(&profile, 0);
        assert!(stages.get(&profile, 0).is_none());
        assert_eq!(stages.get(&profile, 1).unwrap().title, "Window B");
        stages
            .stage(&profile, 0, selection(0, 41, 0x101), "Window A".into())
            .unwrap();
        stages.clear_user(&profile, usize::MAX);
        assert!(stages.get(&profile, 0).is_some());

        profile.desktop.as_mut().unwrap().allowed_pids = vec![42, 41, 42];
        stages.prune(&profile);
        assert_eq!(stages.get(&profile, 0).unwrap().title, "Window A");

        profile.seats.swap(0, 1);
        stages.prune(&profile);
        assert_eq!(stages.get(&profile, 0).unwrap().title, "Window B");
        assert_eq!(stages.get(&profile, 1).unwrap().title, "Window A");
        let selections = stages.take_for_start(&profile);
        assert_eq!(
            selections
                .iter()
                .map(|selection| (selection.user, selection.pid))
                .collect::<Vec<_>>(),
            vec![(0, 42), (1, 41)]
        );
        assert!(stages.take_for_start(&profile).is_empty());
    }

    #[test]
    fn scope_changes_prune_source_policy_bounds_and_removed_user() {
        let base = profile();
        let mut stages = StagedWindows::default();
        stages
            .stage(&base, 0, selection(0, 41, 0x101), "Target".into())
            .unwrap();

        for mutate in [
            0, // source device path
            1, // desktop mode
            2, // allowed process scope
            3, // independent keyboards
            4, // bounds mode
            5, // removed seat
            6, // keyboard source path
            7, // ambiguous stable identity
        ] {
            let mut changed = base.clone();
            match mutate {
                0 => changed.seats[0].mouse = "virtual://mouse/99".into(),
                1 => changed.desktop.as_mut().unwrap().mode = mux_core::routing::Mode::Switched,
                2 => changed.desktop.as_mut().unwrap().allowed_pids = vec![42],
                3 => changed.desktop.as_mut().unwrap().multi_keyboard = false,
                4 => changed.seats[0].cursor.bounds_mode = mux_core::desktop::BoundsMode::Region,
                5 => {
                    changed.seats.remove(0);
                }
                6 => changed.seats[0].keyboard = Some("virtual://keyboard/99".into()),
                _ => changed.seats[1].id = changed.seats[0].id.clone(),
            }
            let mut candidate = stages.clone();
            candidate.prune(&changed);
            assert!(candidate.get(&changed, 0).is_none(), "change case {mutate}");
        }
    }

    #[test]
    fn rejects_duplicate_pid_and_never_serializes_staged_identity() {
        let profile = profile();
        let mut stages = StagedWindows::default();
        stages
            .stage(&profile, 0, selection(0, 41, 0x101), "Window A".into())
            .unwrap();
        assert!(
            stages
                .stage(&profile, 1, selection(1, 41, 0x202), "Window B".into())
                .is_err()
        );

        let saved = serde_json::to_string(&profile).unwrap();
        assert!(!saved.contains("Window A"));
        assert!(!saved.contains("13245678901234567890"));
        assert!(!saved.contains("257"));
    }

    #[test]
    fn unrelated_user_edits_keep_stage_and_duplicate_stable_ids_fail_closed() {
        let mut profile = profile();
        let mut stages = StagedWindows::default();
        stages
            .stage(&profile, 0, selection(0, 41, 0x101), "Window A".into())
            .unwrap();

        profile.seats[1].mouse = "virtual://mouse/88".into();
        stages.prune(&profile);
        assert_eq!(stages.get(&profile, 0).unwrap().title, "Window A");

        profile.seats[1].id = profile.seats[0].id.clone();
        stages.prune(&profile);
        assert!(stages.get(&profile, 0).is_none());
        assert!(stages.take_for_start(&profile).is_empty());
    }

    #[test]
    fn rejects_out_of_scope_and_invalid_identity_without_staging() {
        let profile = profile();
        let mut stages = StagedWindows::default();
        assert!(
            stages
                .stage(&profile, 0, selection(0, 99, 0x101), "Outside".into())
                .is_err()
        );
        let mut invalid = selection(0, 41, 0x101);
        invalid.creation_token = "041".into();
        assert!(
            stages
                .stage(&profile, 0, invalid, "Invalid".into())
                .is_err()
        );
        assert!(stages.take_for_start(&profile).is_empty());
    }
}
