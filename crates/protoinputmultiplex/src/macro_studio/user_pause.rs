//! Session-local admission quarantine. Saved native releases remain in the routers.
use super::*;
use crate::routing_engine::UserPauseState;

#[derive(Default)]
pub(super) struct Sources {
    // This ledger survives global cancel: a released native DOWN is not a real source UP.
    pub(super) down: BTreeMap<Owner, bool>,
    pub(super) paused: BTreeSet<usize>,
    last_cleanup: Option<Instant>,
}
fn external_user(owner: &Owner) -> Option<usize> {
    match owner.origin {
        Origin::Manual(user) | Origin::Agent(_, user) => Some(user),
        _ => None,
    }
}
impl Sources {
    pub(super) fn retire_client(&mut self, client: u64) {
        self.down
            .retain(|owner, _| !matches!(owner.origin, Origin::Agent(id, _) if id == client));
    }
    pub(super) fn retire_user(&mut self, user: usize) {
        self.down
            .retain(|owner, _| external_user(owner) != Some(user));
        self.paused.remove(&user);
    }
}
impl Runtime {
    fn source_owner(&self, origin: Origin, action: &Action) -> Option<(Owner, bool)> {
        let Action::Packet { user, packet } = action else {
            return None;
        };
        let seat = self.seats.get(*user)?;
        let mut raw = *packet;
        raw.device = if raw.kind == 0 {
            seat.mouse
        } else {
            seat.keyboard?
        };
        Claims::raw_owner(origin, raw)
    }
    pub(super) fn user_pause_suppresses(
        &self,
        origin: Origin,
        action: &Action,
        router: &dyn Engine,
    ) -> bool {
        router.user_pause_state(action.user()).paused
            || self
                .source_owner(origin, action)
                .is_some_and(|(owner, _)| self.sources.down.get(&owner) == Some(&true))
    }
    /// Source grants remain necessary for suppressed input. No recipient is needed:
    /// these packets can only consume this actor's marker or become quarantined.
    pub(super) fn authorize_paused_source(&self, client: u64, action: &Action) -> Result<()> {
        let user = action.user();
        self.agent_access.check_users(client, user, &[], user)?;
        if let Action::Packet { packet, .. } = action {
            let grant = self
                .agent_access
                .users
                .get(&user)
                .context("Source user is not permitted")?;
            let expected = if packet.kind == 0 {
                Some(grant.mouse)
            } else {
                grant.keyboard
            };
            ensure!(
                expected == Some(packet.device),
                "Input device does not match the permitted source"
            );
        }
        Ok(())
    }
    /// Runs only after syntax/source authorization, before recording or any graph effects.
    pub(super) fn source_gate(
        &mut self,
        origin: Origin,
        action: &Action,
        router: &mut dyn Engine,
    ) -> Result<bool> {
        let paused = router.user_pause_state(action.user()).paused;
        let Some((owner, up)) = self.source_owner(origin, action) else {
            return Ok(paused);
        };
        let quarantined = self.sources.down.get(&owner) == Some(&true);
        if up {
            self.sources.down.remove(&owner);
            if paused || quarantined {
                // Retry only this exact saved claim. Another actor's owner survives.
                self.claims.3.remove(&owner);
                self.claims.2.retain(|candidate| !(candidate.source == owner.source && candidate.scan == owner.scan && candidate.key_flags == owner.key_flags
                    && matches!(candidate.origin, Origin::Forward(program, source) if program == origin.program() && source == action.user())));
                self.claims
                    .release_matching(router, |candidate, _| *candidate == owner)?;
                return Ok(true);
            }
        } else {
            if !self.sources.down.contains_key(&owner) {
                ensure!(
                    self.sources
                        .down
                        .len()
                        .saturating_add(self.claims.1.len())
                        .saturating_add(self.claims.3.len())
                        < mux_core::automation::MAX_CLIP_EVENTS,
                    "External input source capacity reached"
                );
            }
            self.sources.down.insert(owner, paused || quarantined);
        }
        Ok(paused || quarantined)
    }
    fn require_user_pause_compatible(&self, user: usize) -> Result<()> {
        self.require_active(user)?;
        let mapper = self.mapper.snapshot();
        ensure!(
            !(mapper.armed || mapper.cleanup_pending) || !self.mapper_references_user(user),
            "Disarm Input Mapper and finish cleanup before pausing a mapped user"
        );
        ensure!(
            !self.studio_busy(user),
            "Stop this user's program or recording before pausing"
        );
        self.inner.require_selection_idle(user)?;
        let state = self.inner.state(false);
        ensure!(
            !state
                .edges
                .iter()
                .any(|edge| edge.source == user || edge.target == user),
            "Remove replication involving this user before pausing"
        );
        ensure!(
            !state
                .forwarding_edges
                .iter()
                .any(|edge| edge.source == user || edge.target == user),
            "Remove forwarding involving this user before pausing"
        );
        let seat = &self.seats[user];
        ensure!(
            !self.claims.1.iter().any(|(owner, pair)| {
                !matches!(owner.origin, Origin::Manual(_) | Origin::Agent(_, _))
                    && (pair.path.contains(&user)
                        || owner.source.device == seat.mouse
                        || Some(owner.source.device) == seat.keyboard
                        || pair.output.is_some_and(|p| {
                            p.device == seat.mouse || Some(p.device) == seat.keyboard
                        }))
            }),
            "Finish derived input cleanup before pausing this user"
        );
        Ok(())
    }
    fn paused_claims(&self, user: usize) -> usize {
        self.claims
            .1
            .keys()
            .filter(|owner| external_user(owner) == Some(user))
            .count()
    }
    pub fn user_pause_state(&self, user: usize, router: &dyn Engine) -> UserPauseState {
        let mut state = router.user_pause_state(user);
        if state.paused {
            state.pending_releases = state.pending_releases.max(self.paused_claims(user));
        }
        if state.supported {
            if let Err(error) = self.require_user_pause_compatible(user) {
                state.supported = false;
                state.error = Some(format!("{error:#}"));
            } else if state.paused && router.paused() {
                state
                    .error
                    .get_or_insert_with(|| "Resume the session before resuming this user".into());
            }
        }
        state
    }
    pub fn user_pause_states(&self, router: &dyn Engine) -> Vec<UserPauseState> {
        self.active
            .iter()
            .enumerate()
            .filter(|(_, active)| **active)
            .map(|(user, _)| self.user_pause_state(user, router))
            .collect()
    }
    pub fn user_pause(
        &mut self,
        user: usize,
        paused: bool,
        router: &mut dyn Engine,
    ) -> Result<UserPauseState> {
        self.require_user_pause_compatible(user)?;
        let state = router.user_pause_state(user);
        ensure!(
            state.supported,
            "{}",
            state
                .error
                .as_deref()
                .unwrap_or("Per-user pause is unavailable")
        );
        if paused {
            // Quarantine before the first fallible release, across every external actor.
            self.sources.paused.insert(user);
            for (owner, quarantined) in &mut self.sources.down {
                if external_user(owner) == Some(user) {
                    *quarantined = true;
                }
            }
            let native = router.pause_user(user, true);
            if !router.user_pause_state(user).paused {
                self.sources.paused.remove(&user);
                native?;
                anyhow::bail!("Router did not acknowledge this user's pause");
            }
            let claims = self
                .claims
                .release_matching(router, |owner, _| external_user(owner) == Some(user));
            native.and(claims)?;
        } else {
            ensure!(
                !router.paused(),
                "Resume the session before resuming this user"
            );
            ensure!(
                self.paused_claims(user) == 0 || !state.paused,
                "This user's saved input cleanup remains pending"
            );
            router.pause_user(user, false)?;
            self.sources.paused.remove(&user);
        }
        Ok(self.user_pause_state(user, router))
    }
    pub(super) fn retry_user_pause_claims(&mut self, router: &mut dyn Engine) {
        let now = Instant::now();
        if self
            .sources
            .last_cleanup
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(250))
        {
            return;
        }
        self.sources.last_cleanup = Some(now);
        for user in self.sources.paused.iter().copied() {
            // Native routers retain and retry their saved recipient UPs independently.
            // A failure here remains visible as pending; it must not cancel user B.
            let _ = self
                .claims
                .release_matching(router, |owner, _| external_user(owner) == Some(user));
        }
    }
    pub(super) fn validate_paused_studio(
        &self,
        command: &Command,
        router: &dyn Engine,
    ) -> Result<()> {
        let user = match command {
            Command::Record { user, active: true } | Command::Play { user, .. } => Some(*user),
            _ => None,
        };
        ensure!(
            user.is_none_or(|u| !router.user_pause_state(u).paused),
            "Resume this user before recording or playback"
        );
        Ok(())
    }
    pub(super) fn validate_paused_automation(
        &self,
        command: &automation_runtime::Command,
        router: &dyn Engine,
    ) -> Result<()> {
        use automation_runtime::Command as C;
        let paused = |u| router.user_pause_state(u).paused;
        let blocked = match command {
            C::RecordStart | C::Play { .. } => (0..self.seats.len()).any(paused),
            C::Replication { edges } | C::Graph { edges, .. } => {
                edges.iter().any(|e| paused(e.source) || paused(e.target))
            }
            C::Forwarding { edges, .. } => {
                edges.iter().any(|e| paused(e.source) || paused(e.target))
            }
            _ => false,
        };
        ensure!(
            !blocked,
            "Resume affected users before starting this automation"
        );
        Ok(())
    }
}
