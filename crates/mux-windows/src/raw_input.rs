//! Observe mice and keyboard activity without suppressing input or retaining key contents.
use anyhow::{Context, Result, bail, ensure};
use mux_core::{Device, DeviceKind};
use std::{
    mem::size_of,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
};
use windows_sys::Win32::{
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::*, WindowsAndMessaging::*},
};

pub fn devices() -> Result<Vec<Device>> {
    // A device may arrive between the count and list calls; retry the entire snapshot.
    for _ in 0..3 {
        let mut count = 0;
        unsafe {
            ensure!(
                GetRawInputDeviceList(
                    null_mut(),
                    &mut count,
                    size_of::<RAWINPUTDEVICELIST>() as u32
                ) != u32::MAX,
                "device count: {}",
                std::io::Error::last_os_error()
            );
            if count == 0 {
                return Ok(Vec::new());
            }
            let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
            let read = GetRawInputDeviceList(
                list.as_mut_ptr(),
                &mut count,
                size_of::<RAWINPUTDEVICELIST>() as u32,
            );
            if read == u32::MAX {
                continue;
            }
            let mut result = Vec::new();
            for item in &list[..read as usize] {
                let kind = match item.dwType {
                    RIM_TYPEMOUSE => DeviceKind::Mouse,
                    RIM_TYPEKEYBOARD => DeviceKind::Keyboard,
                    _ => continue,
                };
                let mut length = 0;
                if GetRawInputDeviceInfoW(item.hDevice, RIDI_DEVICENAME, null_mut(), &mut length)
                    == u32::MAX
                {
                    continue;
                }
                let mut name = vec![0u16; length as usize + 1];
                if GetRawInputDeviceInfoW(
                    item.hDevice,
                    RIDI_DEVICENAME,
                    name.as_mut_ptr().cast(),
                    &mut length,
                ) == u32::MAX
                {
                    continue;
                }
                let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
                result.push(Device {
                    path: String::from_utf16_lossy(&name[..end]),
                    kind,
                    handle: item.hDevice as usize,
                });
            }
            result.sort_by(|a, b| a.path.cmp(&b.path));
            return Ok(result);
        }
    }
    bail!(
        "device enumeration kept changing or failed: {}",
        std::io::Error::last_os_error()
    )
}

#[derive(Debug)]
pub enum Event {
    Mouse {
        handle: usize,
        x: i32,
        y: i32,
        absolute: bool,
        buttons: u16,
    },
    Keyboard {
        handle: usize,
    },
    DevicesChanged,
    Error(String),
}

static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
struct CaptureLease;
impl Drop for CaptureLease {
    fn drop(&mut self) {
        CAPTURE_ACTIVE.store(false, Ordering::Release);
    }
}

pub struct Capture {
    _lease: CaptureLease,
    pub events: Receiver<Event>,
    dropped: Arc<AtomicU64>,
    hwnd: usize,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start() -> Result<Self> {
        ensure!(
            CAPTURE_ACTIVE
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok(),
            "a raw input observer is already active in this process"
        );
        let lease = CaptureLease;
        let (send, events) = mpsc::sync_channel(4096);
        let (ready, ready_rx) = mpsc::sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let lost = dropped.clone();
        let thread = thread::Builder::new()
            .name("raw-input".into())
            .spawn(move || {
                // All window operations run on the owning thread; handles cross threads as integers.
                unsafe {
                    let instance = GetModuleHandleW(null());
                    let class: Vec<u16> = "ProtoInputMultiPlex.RawInput\0".encode_utf16().collect();
                    let wc = WNDCLASSW {
                        lpfnWndProc: Some(DefWindowProcW),
                        hInstance: instance,
                        lpszClassName: class.as_ptr(),
                        ..Default::default()
                    };
                    // A second capture can reuse this process-wide class.
                    RegisterClassW(&wc);
                    let hwnd = CreateWindowExW(
                        0,
                        class.as_ptr(),
                        class.as_ptr(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        HWND_MESSAGE,
                        null_mut(),
                        instance,
                        null(),
                    );
                    if hwnd.is_null() {
                        let _ = ready.send(Err(std::io::Error::last_os_error().to_string()));
                        return;
                    }
                    let devices = [2, 6].map(|usage| RAWINPUTDEVICE {
                        usUsagePage: 1,
                        usUsage: usage,
                        dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
                        hwndTarget: hwnd,
                    });
                    if RegisterRawInputDevices(
                        devices.as_ptr(),
                        devices.len() as u32,
                        size_of::<RAWINPUTDEVICE>() as u32,
                    ) == 0
                    {
                        let _ = ready.send(Err(std::io::Error::last_os_error().to_string()));
                        DestroyWindow(hwnd);
                        return;
                    }
                    if ready.send(Ok(hwnd as usize)).is_err() {
                        DestroyWindow(hwnd);
                        return;
                    }
                    let mut msg = MSG::default();
                    loop {
                        let status = GetMessageW(&mut msg, null_mut(), 0, 0);
                        if status <= 0 {
                            if status == -1 {
                                emit(
                                    &send,
                                    &lost,
                                    Event::Error(std::io::Error::last_os_error().to_string()),
                                );
                            }
                            break;
                        }
                        if msg.message == WM_APP {
                            break;
                        }
                        if msg.message == WM_INPUT {
                            // RAWINPUT is aligned; keyboard contents never enter the event channel.
                            let mut input = RAWINPUT::default();
                            let mut bytes = size_of::<RAWINPUT>() as u32;
                            let copied = GetRawInputData(
                                msg.lParam as HRAWINPUT,
                                RID_INPUT,
                                (&mut input as *mut RAWINPUT).cast(),
                                &mut bytes,
                                size_of::<RAWINPUTHEADER>() as u32,
                            );
                            if copied == u32::MAX {
                                emit(
                                    &send,
                                    &lost,
                                    Event::Error(format!(
                                        "Read input: {}",
                                        std::io::Error::last_os_error()
                                    )),
                                );
                            } else if copied as usize
                                >= size_of::<RAWINPUTHEADER>() + size_of::<RAWMOUSE>()
                                && input.header.dwType == RIM_TYPEMOUSE
                            {
                                let mouse = input.data.mouse;
                                emit(
                                    &send,
                                    &lost,
                                    Event::Mouse {
                                        handle: input.header.hDevice as usize,
                                        x: mouse.lLastX,
                                        y: mouse.lLastY,
                                        absolute: mouse.usFlags & 1 != 0,
                                        buttons: mouse.Anonymous.Anonymous.usButtonFlags,
                                    },
                                );
                            } else if copied as usize
                                >= size_of::<RAWINPUTHEADER>() + size_of::<RAWKEYBOARD>()
                                && input.header.dwType == RIM_TYPEKEYBOARD
                            {
                                emit(
                                    &send,
                                    &lost,
                                    Event::Keyboard {
                                        handle: input.header.hDevice as usize,
                                    },
                                );
                            }
                        } else if msg.message == WM_INPUT_DEVICE_CHANGE {
                            emit(&send, &lost, Event::DevicesChanged);
                        }
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg); // DefWindowProc performs foreground raw-input cleanup.
                    }
                    let remove = [2, 6].map(|usage| RAWINPUTDEVICE {
                        usUsagePage: 1,
                        usUsage: usage,
                        dwFlags: RIDEV_REMOVE,
                        hwndTarget: null_mut(),
                    });
                    RegisterRawInputDevices(
                        remove.as_ptr(),
                        remove.len() as u32,
                        size_of::<RAWINPUTDEVICE>() as u32,
                    );
                    DestroyWindow(hwnd);
                }
            })
            .context("start raw input thread")?;
        match ready_rx
            .recv()
            .context("raw input thread exited during startup")?
        {
            Ok(hwnd) => Ok(Self {
                _lease: lease,
                events,
                dropped,
                hwnd,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                bail!("register raw input: {error}")
            }
        }
    }
    pub fn dropped_packets(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

fn emit(sender: &SyncSender<Event>, dropped: &AtomicU64, event: Event) {
    if sender.try_send(event).is_err() {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.hwnd as _, WM_APP, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
