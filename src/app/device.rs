use super::*;

impl App {
    pub(crate) fn handle_device_key(&mut self, mut view: DeviceView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        match key.code {
            KeyCode::Esc => self.manager_return_mode(),
            _ if hotkeys.tools.matches(key) => AppMode::Tools(ToolsView { selected: 0 }),
            _ if hotkeys.quit.matches(key) => AppMode::Browser,
            KeyCode::Down => {
                if !view.devices.is_empty() {
                    view.selected = (view.selected + 1).min(view.devices.len() - 1);
                }
                AppMode::Devices(view)
            }
            _ if hotkeys.down.matches(key) => {
                if !view.devices.is_empty() {
                    view.selected = (view.selected + 1).min(view.devices.len() - 1);
                }
                AppMode::Devices(view)
            }
            KeyCode::Up => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Devices(view)
            }
            _ if hotkeys.up.matches(key) => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Devices(view)
            }
            _ if hotkeys.refresh.matches(key) => {
                let selected_source = view
                    .devices
                    .get(view.selected)
                    .map(|device| device.source.clone());
                self.start_device_refresh(selected_source);
                AppMode::Devices(view)
            }
            _ if hotkeys.device_eject.matches(key) => {
                let Some(device) = view.devices.get(view.selected) else {
                    return AppMode::Devices(view);
                };
                if device.system_protected || !device.ejectable || device.eject_blocked {
                    return AppMode::Devices(view);
                }
                let steps = if !device.encrypted && device.is_mounted() {
                    "The filesystem will be unmounted and the drive safely ejected."
                } else if !device.encrypted {
                    "The drive will be safely ejected."
                } else if device.is_mounted() {
                    "The volume will be unmounted, locked, and safely ejected."
                } else if !device.is_locked() {
                    "The volume will be locked and safely ejected."
                } else {
                    "The device will be safely ejected."
                };
                AppMode::Prompt(Prompt::ConfirmLuks {
                    action: LuksAction::Eject {
                        source: device.source.clone(),
                        drive: device.drive.clone(),
                    },
                    title: "Eject removable device".into(),
                    body: format!(
                        "Device: {}\nPhysical drive: {}\n\n{}",
                        device.source.display(),
                        device.drive.display(),
                        steps,
                    ),
                })
            }
            KeyCode::Enter => self.device_action(view),
            _ if hotkeys.device_action.matches(key) || hotkeys.device_unmount.matches(key) => {
                self.device_action(view)
            }
            _ => AppMode::Devices(view),
        }
    }

    pub(crate) fn device_action(&mut self, view: DeviceView) -> AppMode {
        let Some(device) = view.devices.get(view.selected) else {
            return AppMode::Devices(view);
        };
        if device.system_protected {
            self.status = format!(
                "{} is a protected system device; disk actions are disabled",
                device.source.display()
            );
            return AppMode::Devices(view);
        }
        if !device.encrypted {
            if device.filesystem.is_none() {
                self.status = format!(
                    "{} has no directly mountable filesystem",
                    device.source.display()
                );
                return AppMode::Devices(view);
            }
            let (action, title, body) = if device.is_mounted() {
                (
                    LuksAction::UnmountFilesystem {
                        source: device.source.clone(),
                    },
                    "Unmount filesystem",
                    format!(
                        "Device: {}\nMounted at: {}\n\nOpen files and programs using this filesystem must be closed. The unmount is cancelled if the device is busy.",
                        device.source.display(),
                        device.mountpoints.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")
                    ),
                )
            } else {
                (
                    LuksAction::MountFilesystem {
                        source: device.source.clone(),
                    },
                    "Mount filesystem",
                    format!("Device: {}", device.source.display()),
                )
            };
            return AppMode::Prompt(Prompt::ConfirmLuks {
                action,
                title: title.into(),
                body,
            });
        }
        if device.is_locked() {
            AppMode::Prompt(Prompt::LuksPassphrase {
                source: device.source.clone(),
                label: device.label.clone(),
                size: device.size,
                input: SecretInput::default(),
                error: None,
            })
        } else if device.is_mounted() {
            let Some(mapping) = device.mapping.clone() else {
                self.status = format!(
                    "{} changed state; refresh the device list and try again",
                    device.source.display()
                );
                return AppMode::Devices(view);
            };
            AppMode::Prompt(Prompt::ConfirmLuks {
                        action: LuksAction::UnmountAndLock {
                            source: device.source.clone(),
                            mapping,
                        },
                        title: "Unmount and lock LUKS volume".into(),
                        body: format!(
                            "Device: {}\nMapping: {}\nMounted at: {}\n\nThe volume will only be locked if unmounting succeeds.",
                            device.source.display(),
                            device.mapping.as_ref().map(|path| path.display().to_string()).unwrap_or_else(|| "—".into()),
                            device.mountpoints.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", "),
                        ),
                    })
        } else {
            let Some(mapping) = device.mapping.clone() else {
                self.status = format!(
                    "{} changed state; refresh the device list and try again",
                    device.source.display()
                );
                return AppMode::Devices(view);
            };
            AppMode::Prompt(Prompt::ConfirmLuks {
                action: LuksAction::Mount { mapping },
                title: "Mount unlocked LUKS volume".into(),
                body: format!(
                    "Device: {}\nMapping: {}",
                    device.source.display(),
                    device
                        .mapping
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
            })
        }
    }

    pub(super) fn start_luks(&mut self, action: LuksAction, retry: Option<LuksRetry>) {
        let (label, current) = match &action {
            LuksAction::UnlockAndMount { source, .. } => {
                ("Unlocking and mounting volume", source.clone())
            }
            LuksAction::Mount { mapping } => ("Mounting volume", mapping.clone()),
            LuksAction::MountFilesystem { source } => ("Mounting filesystem", source.clone()),
            LuksAction::UnmountFilesystem { source } => ("Unmounting filesystem", source.clone()),
            LuksAction::UnmountAndLock { source, .. } => {
                ("Unmounting and locking volume", source.clone())
            }
            LuksAction::Eject { drive, .. } => ("Safely ejecting device", drive.clone()),
        };
        let started_at = Instant::now();
        self.progress = ProgressState {
            label: label.into(),
            phase: Some("Preparing device operation".into()),
            current: Some(current),
            cancellable: false,
            started_at: Some(started_at),
            phase_started_at: Some(started_at),
            ..ProgressState::default()
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = luks::execute_with_progress(&action, |label| {
                let _ = sender.send(LuksUpdate::Phase {
                    label,
                    started_at: Instant::now(),
                });
            });
            let _ = sender.send(LuksUpdate::Finished(result));
        });
        self.luks_operation = Some(RunningLuks {
            receiver,
            retry,
            started_at,
        });
    }
    #[allow(dead_code)]
    pub(crate) fn open_devices(&mut self) -> AppMode {
        if cfg!(test) {
            self.last_device_refresh = Instant::now();
            return match luks::discover() {
                Ok(devices) => AppMode::Devices(DeviceView {
                    devices,
                    selected: 0,
                }),
                Err(error) => AppMode::Prompt(Prompt::Message {
                    title: "Storage devices unavailable".into(),
                    body: error.to_string(),
                }),
            };
        }
        self.start_device_refresh(None);
        AppMode::Devices(DeviceView {
            devices: Vec::new(),
            selected: 0,
        })
    }

    pub(crate) fn device_manager_available(&self) -> bool {
        command_available("lsblk")
    }
}
