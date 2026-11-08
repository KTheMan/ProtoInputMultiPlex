//! Mapper delivery reuses supervised claims, permissions and native recipients.
use super::*;
use crate::mapper_runtime::{Actor, Backend, OutputScope, PlannedEffect, Quantized, SinkOwner};
use crate::routing_engine::ProgramOrigin;
use mux_core::mapper::{Disposition, OutputChannel, Source};

pub(super) struct Adapter<'a> {
    pub router: &'a mut dyn Engine,
    pub claims: &'a mut Claims,
    pub raw_sources: &'a user_pause::Sources,
    pub legacy: &'a mut automation_runtime::Runtime,
    pub outputs: &'a crate::output_runtime::Rules,
    pub access: Arc<AgentAccess>,
    pub seats: &'a [ResolvedSeat],
    pub identities: &'a [String],
    pub active: &'a [bool],
    /// Derived from persisted interface paths, never inferred from raw handles.
    pub virtual_users: &'a [bool],
    pub capture_pids: &'a BTreeSet<u32>,
    pub independent_busy: bool,
    pub record: bool,
    pub strict_original: bool,
    pub(super) checked_outputs: Vec<OutputScope>,
}

impl Adapter<'_> {
    fn supervised<T>(&mut self, operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.router.mapper_delivery(true)?;
        let result = operation(self);
        let reset = self.router.mapper_delivery(false);
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                reset?;
                Ok(value)
            }
        }
    }
    fn representative(&self, scope: &OutputScope) -> Result<Quantized> {
        Ok(match scope.channel {
            OutputChannel::TypeText => Quantized::Text(" ".into()),
            OutputChannel::Position => {
                let (x, y) = self.router.pointer(scope.user)?;
                Quantized::Position([x, y])
            }
            OutputChannel::Delta => Quantized::Delta([0, 0]),
            OutputChannel::Wheel { horizontal } => Quantized::Wheel {
                horizontal,
                delta: 1,
            },
            OutputChannel::KeyHold { key } | OutputChannel::KeyPulse { key } => Quantized::Key {
                key,
                held: true,
                pulse: false,
            },
            OutputChannel::ButtonHold { button } | OutputChannel::ButtonPulse { button } => {
                Quantized::Button {
                    button,
                    held: true,
                    pulse: false,
                }
            }
        })
    }
    fn user(&self, user: usize) -> Result<&ResolvedSeat> {
        ensure!(
            self.active.get(user) == Some(&true),
            "Mapper user is not active"
        );
        self.seats.get(user).context("Unknown mapper user")
    }
    fn identity(&self, identity: &str) -> Result<usize> {
        self.identities
            .iter()
            .position(|id| id == identity)
            .context("Mapper user identity is unavailable")
    }
    fn virtual_user(&self, user: usize) -> Result<&ResolvedSeat> {
        let seat = self.user(user)?;
        ensure!(
            self.virtual_users.get(user) == Some(&true),
            "Input Mapper requires virtual mouse/keyboard users"
        );
        Ok(seat)
    }
    fn key_packet(&self, user: usize, key: u16, up: bool) -> Result<Packet> {
        let keyboard = self
            .virtual_user(user)?
            .keyboard
            .context("Mapper key needs a paired virtual keyboard")?;
        let packet = mux_windows::keyboard::packet_code(keyboard, u32::from(key), up)?;
        packet.validate()?;
        Ok(packet)
    }
    fn text_packets(&self, user: usize, text: &str) -> Result<Vec<Packet>> {
        ensure!(
            mux_core::mapper::type_text_cost(text)? != 0,
            "Mapper planned text is empty"
        );
        let keyboard = self
            .virtual_user(user)?
            .keyboard
            .context("Mapper text needs a paired virtual keyboard")?;
        mux_core::emulated::text_packets(keyboard, text)
    }
    fn packet(&self, user: usize, effect: &Quantized) -> Result<Option<Packet>> {
        let seat = self.virtual_user(user)?;
        let packet = match *effect {
            Quantized::Text(_) => anyhow::bail!("Mapper text requires scoped sequence delivery"),
            Quantized::Position(_) => return Ok(None),
            Quantized::Delta([x, y]) => Packet::motion(seat.mouse, x, y),
            Quantized::Wheel {
                horizontal: false,
                delta,
            } => Packet::wheel(seat.mouse, delta),
            Quantized::Wheel {
                horizontal: true,
                delta,
            } => Packet::horizontal_wheel(seat.mouse, delta),
            Quantized::Key { key, held, .. } => return self.key_packet(user, key, !held).map(Some),
            Quantized::Button { button, held, .. } => {
                ensure!((1..=5).contains(&button), "Mapper button is outside 1..5");
                Packet::button(seat.mouse, u32::from(button - 1), !held)
            }
        };
        packet.validate()?;
        Ok(Some(packet))
    }
    fn canonical(&self, action: &Action) -> Result<Action> {
        let mut action = action.clone();
        if let Action::Packet { user, packet } = &mut action {
            let seat = self.user(*user)?;
            packet.device = if packet.kind == 0 {
                seat.mouse
            } else {
                seat.keyboard.context("Input source has no keyboard")?
            };
        }
        Ok(action)
    }
    fn check_effect(&self, owner: &SinkOwner, effect: &Quantized) -> Result<()> {
        self.virtual_user(owner.user)?;
        if let Quantized::Text(text) = effect {
            ensure!(
                self.router.keyboard_enabled(owner.user),
                "Mapper output keyboard is disabled"
            );
            for raw in self.text_packets(owner.user, text)? {
                if let Some(output) = self.outputs.translate(raw)? {
                    ensure!(
                        self.router.packet_window(owner.user, output)?.is_some(),
                        "Mapper text needs a selected input recipient"
                    );
                }
                if let Some((client, source)) = owner.origin.agent() {
                    check_agent_packet(
                        &self.access,
                        self.outputs,
                        self.router,
                        client,
                        source,
                        &owner.source_users,
                        owner.user,
                        raw,
                    )?;
                }
            }
            return Ok(());
        }
        if effect.release() {
            return Ok(());
        }
        let packet = self.packet(owner.user, effect)?;
        if let Some(raw) = packet
            && let Some(output) = self.outputs.translate(raw)?
            && !(output.kind == 0 && output.flags == 0)
        {
            ensure!(
                self.router.packet_window(owner.user, output)?.is_some(),
                "Mapper output needs a selected input recipient"
            );
        }
        if let Some((client, source)) = owner.origin.agent() {
            self.access
                .check_users(client, source, &owner.source_users, owner.user)?;
            if let Quantized::Position([x, y]) = *effect {
                let window = self.router.movement_window(owner.user, x, y)?;
                self.access.check_motion_target(client, window)?;
                self.access
                    .check_pending_attachment(client, self.router, owner.user)?;
            } else if let Some(packet) = packet {
                check_agent_packet(
                    &self.access,
                    self.outputs,
                    self.router,
                    client,
                    source,
                    &owner.source_users,
                    owner.user,
                    packet,
                )?;
            }
        }
        if matches!(effect, Quantized::Key { .. }) {
            ensure!(
                self.router.keyboard_enabled(owner.user),
                "Mapper output keyboard is disabled"
            );
        }
        Ok(())
    }
    fn timer_recipient(&self, user: usize, effect: &Quantized) -> Result<Window> {
        let window = if let Quantized::Position([x, y]) = *effect {
            self.router.movement_window(user, x, y)?
        } else {
            let raw = if let Quantized::Text(text) = effect {
                self.text_packets(user, text)?[0]
            } else {
                self.packet(user, effect)?
                    .context("timer output packet unavailable")?
            };
            let output = self
                .outputs
                .translate(raw)?
                .context("timer output is disabled")?;
            self.router.packet_window(user, output)?
        };
        window.context("Timer requires a current virtual input recipient")
    }
    fn emit_packet(&mut self, owner: &SinkOwner, raw: Packet) -> Result<()> {
        let origin = Origin::Mapper(owner.origin, owner.run, owner.sink);
        let output = self.outputs.translate(raw)?;
        self.claims
            .input_output(origin, raw, output, &owner.source_users, self.router)
    }
    fn selected_actor(program: ProgramOrigin, actor: Option<Actor>) -> bool {
        match actor {
            None => true,
            Some(Actor::Manual) => matches!(program, ProgramOrigin::Manual(_)),
            Some(Actor::Agent(client)) => program.agent().is_some_and(|(id, _)| id == client),
        }
    }
}

impl Backend for Adapter<'_> {
    fn validate_definition(&mut self, sources: &[Source], outputs: &[OutputScope]) -> Result<()> {
        for source in sources {
            let user = self.identity(source.user())?;
            self.virtual_user(user)?;
            if let Source::Key { key, .. } = source {
                self.key_packet(user, *key, false)?;
            }
        }
        for output in outputs {
            self.virtual_user(output.user)?;
            if output.channel == OutputChannel::TypeText {
                self.text_packets(output.user, " ")?;
            }
            if let OutputChannel::KeyHold { key } | OutputChannel::KeyPulse { key } =
                &output.channel
            {
                self.key_packet(output.user, *key, false)?;
            }
        }
        self.checked_outputs = outputs.to_vec();
        Ok(())
    }
    fn validate_arm(&mut self, sources: &[Source], users: &[usize]) -> Result<()> {
        ensure!(
            self.router.forwarding_supported(),
            "Input Mapper actuation requires automatic virtual desktop routing"
        );
        ensure!(
            !self.router.paused(),
            "Resume routing before arming Input Mapper"
        );
        let legacy = self.legacy.state(false);
        ensure!(
            legacy.edges.is_empty() && legacy.forwarding_edges.is_empty(),
            "Clear replication and input forwarding before arming Input Mapper"
        );
        ensure!(
            !self.independent_busy
                && !legacy.recording
                && legacy.playback.is_none_or(|play| play.state != "playing"),
            "Stop recording and playback before arming Input Mapper"
        );
        for &user in users {
            self.virtual_user(user)?;
            ensure!(
                !self.router.user_pause_state(user).paused,
                "Resume mapped users before arming Input Mapper"
            );
        }
        for source in sources {
            let user = self.identity(source.user())?;
            let seat = self.virtual_user(user)?;
            let raw = match source {
                Source::Key { key, .. } => {
                    ensure!(
                        self.router.keyboard_enabled(user),
                        "Mapper source keyboard is disabled"
                    );
                    Some((
                        seat.keyboard.context("Mapper source needs a keyboard")?,
                        1,
                        u32::from(*key),
                    ))
                }
                Source::Button { button, .. } => Some((seat.mouse, 0, u32::from(button - 1))),
                _ => None,
            };
            if let Some((device, kind, code)) = raw {
                ensure!(
                    !self
                        .claims
                        .1
                        .keys()
                        .chain(self.claims.3.keys())
                        .chain(self.raw_sources.down.keys())
                        .any(|owner| owner.source.device == device
                            && owner.source.kind == kind
                            && owner.source.code == code),
                    "Release mapper source keys/buttons before arming"
                );
            }
        }
        for output in &self.checked_outputs {
            if matches!(
                output.channel,
                OutputChannel::KeyHold { .. }
                    | OutputChannel::KeyPulse { .. }
                    | OutputChannel::TypeText
            ) {
                ensure!(
                    self.router.keyboard_enabled(output.user),
                    "Mapper output keyboard is disabled"
                );
            }
        }
        Ok(())
    }
    fn authorize(
        &mut self,
        origin: ProgramOrigin,
        source_user: usize,
        outputs: &[OutputScope],
    ) -> Result<()> {
        self.user(source_user)?;
        if let Some((client, source)) = origin.agent() {
            ensure!(
                source == source_user,
                "Mapper source origin does not match its user"
            );
            self.access.check_users(client, source, &[], source)?;
            for output in outputs {
                self.access
                    .check_users(client, source, &output.source_users, output.user)?;
            }
        }
        for output in outputs {
            let owner = SinkOwner {
                origin,
                run: 0,
                sink: output.sink,
                user: output.user,
                source_users: output.source_users.clone(),
            };
            self.check_effect(&owner, &self.representative(output)?)?;
        }
        Ok(())
    }
    fn health(&mut self, outputs: &[OutputScope]) -> Result<()> {
        for output in outputs {
            let owner = SinkOwner {
                origin: ProgramOrigin::Manual(output.user),
                run: 0,
                sink: output.sink,
                user: output.user,
                source_users: output.source_users.clone(),
            };
            self.check_effect(&owner, &self.representative(output)?)?;
        }
        Ok(())
    }
    fn capture_authority(
        &mut self,
        origin: ProgramOrigin,
        source_user: usize,
        capture_user: usize,
        source_users: &[usize],
    ) -> Result<crate::mapper_runtime::CaptureAuthority> {
        self.virtual_user(capture_user)?;
        ensure!(
            self.router.forwarding_supported() && !self.router.paused(),
            "Mapper capture requires active automatic virtual desktop routing"
        );
        ensure!(
            !self.router.user_pause_state(capture_user).paused,
            "Mapper capture user is paused"
        );
        let window = self
            .router
            .selected_window(capture_user)?
            .context("Select a Mapper capture application window first")?;
        ensure!(
            !self.capture_pids.is_empty() && self.capture_pids.contains(&window.pid),
            "Mapper capture target is outside the explicit session capture scope"
        );
        let generation = if let Some((client, source)) = origin.agent() {
            ensure!(
                source == source_user,
                "Mapper capture source origin changed"
            );
            ensure!(
                self.access.capture && self.access.clients.contains(&client),
                "Mapper capture permission is disabled or its client was retired"
            );
            self.access
                .check_users(client, source, source_users, capture_user)?;
            self.access.check_target(client, Some(window))?;
            self.access.generation
        } else {
            ensure!(
                matches!(origin, ProgramOrigin::Manual(user) if user == source_user),
                "Mapper capture requires an admitted manual or connected-agent origin"
            );
            0
        };
        Ok(crate::mapper_runtime::CaptureAuthority { window, generation })
    }
    fn pointer(&mut self, user: usize) -> Result<(i32, i32)> {
        self.router.pointer(user)
    }
    fn timer_windows(&mut self, outputs: &[OutputScope]) -> Result<Vec<Window>> {
        ensure!(!self.router.paused(), "timer routing is paused");
        outputs
            .iter()
            .map(|scope| {
                ensure!(
                    !self.router.user_pause_state(scope.user).paused,
                    "timer destination is paused"
                );
                self.timer_recipient(scope.user, &self.representative(scope)?)
            })
            .collect()
    }
    fn watch_timer_targets(
        &self,
        windows: &[Window],
    ) -> Result<Option<std::rc::Rc<mux_windows::targets::ProcessWatch>>> {
        if windows.is_empty() {
            return Ok(None);
        }
        let pids: BTreeSet<_> = windows.iter().map(|window| window.pid).collect();
        let watch = mux_windows::targets::ProcessWatch::open(pids)?;
        ensure!(watch.exited().is_none(), "timer target process exited");
        Ok(Some(std::rc::Rc::new(watch)))
    }
    fn reserve(
        &mut self,
        original: Option<(ProgramOrigin, &Action)>,
        effects: &[PlannedEffect],
    ) -> Result<()> {
        let mut needed = 0usize;
        if let Some((origin, action)) = original
            && let Action::Packet { packet, .. } = self.canonical(action)?
            && let Some((owner, up)) = Claims::raw_owner(original_origin(origin), packet)
            && !up
            && !self.claims.1.contains_key(&owner)
            && !self.claims.3.contains_key(&owner)
        {
            needed += 1;
        }
        for planned in effects {
            self.check_effect(&planned.owner, &planned.effect)?;
            // Text packets are instantaneous; they create no held source claim.
            if matches!(planned.effect, Quantized::Text(_)) {
                continue;
            }
            if !planned.effect.release()
                && let Some(raw) = self.packet(planned.owner.user, &planned.effect)?
                && let Some((owner, up)) = Claims::raw_owner(
                    Origin::Mapper(planned.owner.origin, planned.owner.run, planned.owner.sink),
                    raw,
                )
                && !up
                && !self.claims.1.contains_key(&owner)
            {
                needed += 1;
            }
        }
        ensure!(
            self.claims
                .1
                .len()
                .saturating_add(self.claims.3.len())
                .saturating_add(needed)
                <= mux_core::automation::MAX_CLIP_EVENTS,
            "Input source claim capacity reached"
        );
        Ok(())
    }
    fn original(
        &mut self,
        origin: ProgramOrigin,
        action: &Action,
        disposition: Disposition,
    ) -> Result<()> {
        let action = self.canonical(action)?;
        let consumed = if let Action::Packet { packet, user } = &action {
            self.claims.consume_mapper_source(
                original_origin(origin),
                *packet,
                disposition == Disposition::Consume,
                &[*user],
            )?
        } else {
            disposition == Disposition::Consume
        };
        if consumed {
            return Ok(());
        }
        let operation = |this: &mut Self| {
            let mut engine = ClaimEngine {
                inner: this.router,
                claims: this.claims,
                origin: original_origin(origin),
                agent_access: this.access.clone(),
                outputs: this.outputs,
            };
            this.legacy.action(action, &mut engine, this.record)
        };
        if self.strict_original {
            self.supervised(operation)
        } else {
            operation(self)
        }
    }
    fn check_timer_effect(
        &self,
        owner: &SinkOwner,
        effect: &Quantized,
        expected: Window,
    ) -> Result<()> {
        ensure!(
            self.timer_recipient(owner.user, effect)? == expected,
            "timer effect recipient changed"
        );
        Ok(())
    }
    fn deliver(
        &mut self,
        owner: &SinkOwner,
        effect: &Quantized,
        expected: Option<Window>,
    ) -> Result<()> {
        self.supervised(|this| {
            if let Quantized::Text(text) = effect {
                return this.deliver_text(owner, text, expected);
            }
            if let Some(expected) = expected {
                this.check_timer_effect(owner, effect, expected)?;
            }
            this.deliver_inner(owner, effect)
        })
    }
    fn release(&mut self, run: u64, actor: Option<Actor>) -> Result<()> {
        self.supervised(|this| {
            if actor.is_none() { return this.claims.release_mapper(run, this.router); }
            this.claims.release_matching(this.router, |owner, _| matches!(owner.origin, Origin::Mapper(program, generation, _) if generation == run && Self::selected_actor(program, actor)))
        })
    }
    fn pending(&self, run: u64, actor: Option<Actor>) -> usize {
        self.claims.1.keys().filter(|owner| matches!(owner.origin, Origin::Mapper(program, generation, _) if generation == run && Self::selected_actor(program, actor))).count()
    }
}

impl Adapter<'_> {
    fn deliver_text(
        &mut self,
        owner: &SinkOwner,
        text: &str,
        expected: Option<Window>,
    ) -> Result<()> {
        self.check_effect(owner, &Quantized::Text(text.to_owned()))?;
        let packets = self.text_packets(owner.user, text)?;
        let access = &self.access;
        let outputs = self.outputs;
        let claims = &mut *self.claims;
        let origin = Origin::Mapper(owner.origin, owner.run, owner.sink);
        crate::routing_engine::with_text_recipient(self.router, owner.user, |router| {
            if let Some(expected) = expected {
                ensure!(
                    router.packet_window(owner.user, packets[0])? == Some(expected),
                    "Mapper text recipient changed"
                );
            }
            for raw in packets {
                // Revalidate the retained recipient and current output authority
                // for every unit. A native error may leave an irreversible prefix.
                if let Some((client, source)) = owner.origin.agent() {
                    check_agent_packet(
                        access,
                        outputs,
                        router,
                        client,
                        source,
                        &owner.source_users,
                        owner.user,
                        raw,
                    )?;
                }
                let output = outputs.translate(raw)?;
                claims.input_output(origin, raw, output, &owner.source_users, router)?;
            }
            Ok(())
        })
    }
    fn deliver_inner(&mut self, owner: &SinkOwner, effect: &Quantized) -> Result<()> {
        if effect.release() {
            return self.claims.release_mapper_sink(
                owner.origin,
                owner.run,
                owner.sink,
                self.router,
            );
        }
        self.check_effect(owner, effect)?;
        if let Quantized::Position([x, y]) = *effect {
            return self.router.move_to(owner.user, x, y);
        }
        if let Some(packet) = self.packet(owner.user, effect)? {
            if packet.kind == 0 {
                self.router.note_mouse_activity(owner.user)?;
            }
            self.emit_packet(owner, packet)?;
        }
        if matches!(
            effect,
            Quantized::Key { pulse: true, .. } | Quantized::Button { pulse: true, .. }
        ) {
            self.claims
                .release_mapper_sink(owner.origin, owner.run, owner.sink, self.router)?;
        }
        Ok(())
    }
}

fn original_origin(program: ProgramOrigin) -> Origin {
    match program {
        ProgramOrigin::Manual(user) => Origin::Manual(user),
        ProgramOrigin::Agent(client, user) => Origin::Agent(client, user),
        ProgramOrigin::AgentStudio(owner) => Origin::AgentStudio(owner),
        ProgramOrigin::Composite => Origin::Composite,
        ProgramOrigin::Studio(user) => Origin::Studio(user),
    }
}
