//! One fixed command implementation per Backend; no fallback after attachment.
use super::{control, wire::Command, *};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeKeyCallPhase {
    Entered,
    Returned,
}

fn trace_native_key_call(
    packet: &Packet,
    call: impl FnOnce(),
    mut observe: impl FnMut(NativeKeyCallPhase, &Packet),
) {
    observe(NativeKeyCallPhase::Entered, packet);
    call();
    observe(NativeKeyCallPhase::Returned, packet);
}

fn qualification_packet_matches(packet: &Packet) -> [bool; 6] {
    [
        packet.device == 0xf1000005,
        packet.kind == 1,
        packet.flags == 2,
        packet.x == 29,
        packet.y == 0,
        packet.data == 163,
    ]
}

fn qualification_native_key_call(phase: NativeKeyCallPhase, packet: &Packet, instance: u32) {
    let phase = match phase {
        NativeKeyCallPhase::Entered => "entered",
        NativeKeyCallPhase::Returned => "returned",
    };
    let [
        device_matches,
        kind_matches,
        flags_match,
        scan_matches,
        y_matches,
        key_matches,
    ] = qualification_packet_matches(packet);
    let instance_nonzero = instance != 0;
    qualification_stderr(format_args!(
        "PIM_QUAL_KEY_CALL_V1 phase={phase} instance_nonzero={instance_nonzero} device_matches={device_matches} kind_matches={kind_matches} flags_match={flags_match} scan_matches={scan_matches} y_matches={y_matches} key_matches={key_matches}"
    ));
}

impl Backend {
    /// Development pilot for supervised virtual sessions. Does not load the native loader.
    pub fn load_rust_virtual(folder: &Path, cancel: Arc<AtomicBool>) -> Result<Self> {
        ensure!(
            std::env::var_os(crate::lifecycle::WORKER_ENV).is_some(),
            "Rust virtual control requires a supervised session transaction"
        );
        let lease = BACKEND_OWNER.try_lock().map_err(|_| {
            anyhow::anyhow!("a ProtoInput backend is already active in this process")
        })?;
        let path = folder
            .canonicalize()
            .context("resolve Rust control runtime folder")?;
        let mut folder: Vec<u16> = path.as_os_str().encode_wide().collect();
        ensure!(!folder.contains(&0), "runtime path contains NUL");
        folder.extend(['\\' as u16, 0]);
        let registry = Rc::new(control::Registry::new(cancel));
        Ok(Self {
            native: None,
            lifecycle: Some(crate::lifecycle::Api::rust(registry.clone())),
            rust: Some(registry),
            input_version: 3,
            cursor_arches: [32, 64]
                .into_iter()
                .filter(|bits| {
                    path.join(format!("protoinputmultiplex_cursor_observer{bits}.dll"))
                        .is_file()
                })
                .collect(),
            _lease: lease,
            _thread: PhantomData,
            folder,
            path,
        })
    }

    fn dispatch(
        &self,
        instance: u32,
        command: Command,
        native: impl FnOnce(&NativeApi) -> Result<()>,
    ) -> Result<()> {
        if let Some(registry) = &self.rust {
            registry.send(instance, command)
        } else {
            native(self.native.as_ref().context("native backend unavailable")?)
        }
    }

    fn transfer(
        &self,
        instance: u32,
        command: Command,
        native: impl FnOnce(&NativeApi) -> Result<bool>,
    ) -> Result<bool> {
        if let Some(registry) = &self.rust {
            registry.transfer(instance, command)
        } else {
            native(self.native.as_ref().context("native backend unavailable")?)
        }
    }

    pub(super) fn inject(&self, pid: u32) -> Result<u32> {
        ensure!(
            self.rust.is_none(),
            "Rust control cannot use legacy injection"
        );
        let native = self.native.as_ref().context("native loader unavailable")?;
        Ok(unsafe { (native.inject)(pid, self.folder.as_ptr()) })
    }

    pub(super) fn submit_supported(&self) -> bool {
        self.rust.is_some() || self.native.as_ref().is_some_and(|n| n.submit.is_some())
    }
    pub(super) fn fence_supported(&self) -> bool {
        self.rust.is_some()
            || self
                .native
                .as_ref()
                .is_some_and(|n| n.queue_fence.is_some())
    }
    pub(super) fn window_supported(&self) -> bool {
        self.rust.is_some() || self.native.as_ref().is_some_and(|n| n.window.is_some())
    }
    pub(super) fn bounds_supported(&self) -> bool {
        self.rust.is_some() || self.native.as_ref().is_some_and(|n| n.bounds.is_some())
    }
    pub(super) fn block_supported(&self) -> bool {
        self.rust.is_some()
            || self
                .native
                .as_ref()
                .is_some_and(|n| n.block.is_some() && n.unblock.is_some())
    }
    pub(super) fn cursor_supported(&self) -> bool {
        self.rust.is_some()
            || self.native.as_ref().is_some_and(|n| {
                n.cursor_start.is_some() && n.cursor_associate.is_some() && n.cursor_stop.is_some()
            })
    }
    pub(super) fn keyboard_supported(&self) -> bool {
        self.rust.is_some()
            || self
                .native
                .as_ref()
                .is_some_and(|n| n.keyboard_state.is_some())
    }

    pub(super) fn setup(&self, instance: u32, index: i32) -> Result<()> {
        self.dispatch(instance, Command::State { index }, |n| {
            unsafe {
                (n.setup)(instance, index);
            }
            Ok(())
        })
    }
    pub(super) fn install(&self, instance: u32, hook: u32) -> Result<()> {
        if hook == 2
            && let Some(registry) = &self.rust
        {
            return registry.install_message_filter(instance);
        }
        self.dispatch(
            instance,
            Command::Hook {
                hook,
                install: true,
            },
            |n| {
                unsafe {
                    (n.install)(instance, hook);
                }
                Ok(())
            },
        )
    }
    pub(super) fn uninstall(&self, instance: u32, hook: u32) -> Result<()> {
        self.dispatch(
            instance,
            Command::Hook {
                hook,
                install: false,
            },
            |n| {
                unsafe {
                    (n.uninstall)(instance, hook);
                }
                Ok(())
            },
        )
    }
    pub(super) fn enable_filter(&self, instance: u32, filter: u32) -> Result<()> {
        self.dispatch(
            instance,
            Command::Filter {
                filter,
                enabled: true,
            },
            |n| {
                unsafe {
                    (n.enable_filter)(instance, filter);
                }
                Ok(())
            },
        )
    }
    pub(super) fn disable_filter(&self, instance: u32, filter: u32) -> Result<()> {
        self.dispatch(
            instance,
            Command::Filter {
                filter,
                enabled: false,
            },
            |n| {
                unsafe {
                    (n.disable_filter)(instance, filter);
                }
                Ok(())
            },
        )
    }
    pub(super) fn mouse(&self, instance: u32, mouse: u32) -> Result<()> {
        self.dispatch(
            instance,
            Command::Devices {
                mouse: Some(mouse),
                keyboard: None,
            },
            |n| {
                unsafe {
                    (n.mouse)(instance, mouse);
                }
                Ok(())
            },
        )
    }
    pub(super) fn keyboard(&self, instance: u32, keyboard: u32) -> Result<()> {
        self.dispatch(
            instance,
            Command::Devices {
                mouse: None,
                keyboard: Some(keyboard),
            },
            |n| {
                unsafe {
                    (n.keyboard)(instance, keyboard);
                }
                Ok(())
            },
        )
    }
    pub(super) fn draw(&self, instance: u32, enabled: bool) -> Result<()> {
        self.dispatch(instance, Command::Draw { enabled }, |n| {
            unsafe {
                (n.draw)(instance, enabled);
            }
            Ok(())
        })
    }
    pub(super) fn freeze(&self, instance: u32, enabled: bool) -> Result<()> {
        self.dispatch(instance, Command::Freeze { enabled }, |n| {
            unsafe {
                (n.freeze)(instance, enabled);
            }
            Ok(())
        })
    }
    pub(super) fn messages(
        &self,
        instance: u32,
        wheel: bool,
        buttons: bool,
        motion: bool,
        keyboard: bool,
    ) -> Result<()> {
        self.dispatch(
            instance,
            Command::Messages {
                wheel,
                buttons,
                motion,
                keyboard,
            },
            |n| {
                unsafe {
                    (n.messages)(instance, wheel, buttons, motion, keyboard);
                }
                Ok(())
            },
        )
    }
    // Preserve the pinned Focus export's argument order at the dispatch boundary.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn focus(
        &self,
        instance: u32,
        milliseconds: i32,
        activate: bool,
        activate_app: bool,
        nc_activate: bool,
        set_focus: bool,
        mouse_activate: bool,
    ) -> Result<()> {
        self.dispatch(
            instance,
            Command::Focus {
                milliseconds,
                activate,
                activate_app,
                nc_activate,
                set_focus,
                mouse_activate,
            },
            |n| {
                unsafe {
                    (n.focus)(
                        instance,
                        milliseconds,
                        activate,
                        activate_app,
                        nc_activate,
                        set_focus,
                        mouse_activate,
                    );
                }
                Ok(())
            },
        )
    }
    pub(super) fn stop_focus(&self, instance: u32) -> Result<()> {
        self.dispatch(instance, Command::StopFocus, |n| {
            unsafe {
                (n.stop_focus)(instance);
            }
            Ok(())
        })
    }
    pub(super) fn submit(&self, instance: u32, packet: &Packet) -> Result<()> {
        self.dispatch(
            instance,
            Command::Input {
                device: packet.device,
                kind: packet.kind,
                flags: packet.flags,
                x: packet.x,
                y: packet.y,
                data: packet.data,
            },
            |n| {
                let submit = n.submit.context("backend lacks emulated input support")?;
                static FIRST: AtomicBool = AtomicBool::new(false);
                if qualification_first_key_down(packet, &FIRST) {
                    trace_native_key_call(
                        packet,
                        || unsafe { submit(instance, packet) },
                        |phase, packet| qualification_native_key_call(phase, packet, instance),
                    );
                } else {
                    unsafe { submit(instance, packet) };
                }
                Ok(())
            },
        )
    }
    pub(super) fn window(&self, instance: u32, hwnd: u64) -> Result<()> {
        self.dispatch(instance, Command::Window { hwnd }, |n| {
            unsafe {
                (n.window.context("backend lacks window selection")?)(instance, hwnd);
            }
            Ok(())
        })
    }
    pub(super) fn bounds(&self, instance: u32, allow: bool, extend: bool) -> Result<()> {
        self.dispatch(instance, Command::Bounds { allow, extend }, |n| {
            unsafe {
                (n.bounds.context("backend lacks cursor bounds")?)(instance, allow, extend);
            }
            Ok(())
        })
    }
    pub(super) fn block(&self, instance: u32, message: u32) -> Result<()> {
        self.dispatch(
            instance,
            Command::Block {
                message,
                enabled: true,
            },
            |n| {
                unsafe {
                    (n.block.context("backend lacks message blocking")?)(instance, message);
                }
                Ok(())
            },
        )
    }
    pub(super) fn unblock(&self, instance: u32, message: u32) -> Result<()> {
        self.dispatch(
            instance,
            Command::Block {
                message,
                enabled: false,
            },
            |n| {
                unsafe {
                    (n.unblock.context("backend lacks message unblocking")?)(instance, message);
                }
                Ok(())
            },
        )
    }
    pub(super) fn queue_fence(
        &self,
        instance: u32,
        hwnd: u64,
        event: u64,
        cookie: u64,
    ) -> Result<bool> {
        self.transfer(
            instance,
            Command::Fence {
                hwnd,
                event,
                cookie,
            },
            |n| {
                Ok(unsafe {
                    (n.queue_fence.context("backend lacks queue-fence support")?)(
                        instance, hwnd, event, cookie,
                    )
                })
            },
        )
    }
    pub(super) fn cursor_start(
        &self,
        instance: u32,
        mapping: u64,
        mutex: u64,
        attachment: u64,
    ) -> Result<bool> {
        self.transfer(
            instance,
            Command::CursorStart {
                mapping,
                mutex,
                attachment,
            },
            |n| {
                Ok(unsafe {
                    (n.cursor_start.context("cursor provider unavailable")?)(
                        instance, mapping, mutex, attachment,
                    )
                })
            },
        )
    }
    pub(super) fn cursor_associate(
        &self,
        instance: u32,
        hwnd: u64,
        association: u64,
    ) -> Result<bool> {
        self.transfer(
            instance,
            Command::CursorAssociate { hwnd, association },
            |n| {
                Ok(unsafe {
                    (n.cursor_associate
                        .context("cursor association unavailable")?)(
                        instance, hwnd, association
                    )
                })
            },
        )
    }
    pub(super) fn cursor_stop(&self, instance: u32, attachment: u64) -> Result<bool> {
        self.transfer(instance, Command::CursorStop { attachment }, |n| {
            Ok(
                unsafe {
                    (n.cursor_stop.context("cursor stop unavailable")?)(instance, attachment)
                },
            )
        })
    }
    pub(super) fn keyboard_command(
        &self,
        instance: u32,
        epoch: u64,
        action: u32,
        mapping: u64,
        event: u64,
    ) -> Result<bool> {
        self.transfer(
            instance,
            Command::Keyboard {
                epoch,
                action,
                mapping,
                event,
            },
            |n| {
                Ok(unsafe {
                    (n.keyboard_state
                        .context("keyboard-state backend unavailable")?)(
                        instance, epoch, action, mapping, event,
                    )
                })
            },
        )
    }

    pub(super) fn require_virtual_scope(&self, seats: &[ResolvedSeat]) -> Result<()> {
        if self.rust.is_some() {
            ensure!(
                self.worker_lifecycle().is_some(),
                "Rust virtual control requires supervisor lifecycle"
            );
            ensure!(
                seats.iter().all(virtual_seat),
                "Rust virtual control does not support physical or legacy sessions"
            );
            for seat in seats {
                Packet::motion(seat.mouse, 0, 0).validate()?;
                if let Some(keyboard) = seat.keyboard {
                    Packet::text_unit(keyboard, 1).validate()?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::*;

    #[test]
    fn disabled_or_non_down_calls_do_not_consume_the_single_record() {
        let packet = Packet::key_scanned(0xf1000005, 163, 29, 2, false);
        let first = AtomicBool::new(false);
        assert!(!qualification_claim_key_down(&packet, &first, false));
        assert!(!qualification_claim_key_down(
            &Packet::key_scanned(0xf1000005, 163, 29, 2, true),
            &first,
            true,
        ));
        assert!(!qualification_claim_key_down(
            &Packet::motion(0xf1000004, 1, 0),
            &first,
            true,
        ));
        assert!(qualification_claim_key_down(&packet, &first, true));
        assert!(!qualification_claim_key_down(&packet, &first, true));
    }

    #[test]
    fn first_key_packet_facts_reject_each_tuple_near_miss() {
        let expected = Packet::key_scanned(0xf1000005, 163, 29, 2, false);
        assert_eq!(qualification_packet_matches(&expected), [true; 6]);
        let mut packets = [expected; 6];
        packets[0].device = 0xf1000003;
        packets[1].kind = 0;
        packets[2].flags = 3;
        packets[3].x = 30;
        packets[4].y = 1;
        packets[5].data = 17;
        for (index, packet) in packets.into_iter().enumerate() {
            let mut wanted = [true; 6];
            wanted[index] = false;
            assert_eq!(qualification_packet_matches(&packet), wanted);
        }
    }

    #[test]
    fn native_key_trace_brackets_exactly_one_call_and_keeps_the_packet() {
        let packet = Packet::key_scanned(0xf1000005, 163, 29, 2, false);
        let order = std::cell::RefCell::new(Vec::new());
        trace_native_key_call(
            &packet,
            || order.borrow_mut().push("call"),
            |phase, actual| {
                assert!(std::ptr::eq(actual, &packet));
                order.borrow_mut().push(match phase {
                    NativeKeyCallPhase::Entered => "entered",
                    NativeKeyCallPhase::Returned => "returned",
                });
            },
        );
        assert_eq!(order.into_inner(), ["entered", "call", "returned"]);
    }
}
