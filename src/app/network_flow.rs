use super::*;

impl App {
    pub(crate) fn handle_network_key(&mut self, mut view: NetworkView, key: KeyEvent) -> AppMode {
        let hotkeys = self.config.hotkeys.clone();
        match key.code {
            KeyCode::Esc => AppMode::Browser,
            _ if hotkeys.quit.matches(key) || hotkeys.network_shares.matches(key) => {
                AppMode::Browser
            }
            KeyCode::Down => {
                if !view.shares.is_empty() {
                    view.selected = (view.selected + 1).min(view.shares.len() - 1);
                }
                AppMode::Network(view)
            }
            _ if hotkeys.down.matches(key) => {
                if !view.shares.is_empty() {
                    view.selected = (view.selected + 1).min(view.shares.len() - 1);
                }
                AppMode::Network(view)
            }
            KeyCode::Up => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Network(view)
            }
            _ if hotkeys.up.matches(key) => {
                view.selected = view.selected.saturating_sub(1);
                AppMode::Network(view)
            }
            _ if hotkeys.refresh.matches(key) => {
                let selected_uri = view
                    .shares
                    .get(view.selected)
                    .map(|share| share.address.uri.clone());
                self.start_network_refresh(selected_uri);
                AppMode::Network(view)
            }
            _ if hotkeys.network_add.matches(key) => {
                if self.config.behavior.read_only {
                    self.set_notice("Read-only mode: network connections are disabled");
                    return AppMode::Network(view);
                }
                AppMode::Prompt(Prompt::SmbAddress {
                    input: "smb://".into(),
                    cursor: 6,
                    error: None,
                })
            }
            KeyCode::Enter | KeyCode::Right => self.network_open(view),
            _ if hotkeys.expand.matches(key) => self.network_open(view),
            _ if hotkeys.network_disconnect.matches(key) => {
                if self.config.behavior.read_only {
                    self.set_notice("Read-only mode: network disconnection is disabled");
                    return AppMode::Network(view);
                }
                match view.shares.get(view.selected).cloned() {
                    Some(share) if share.mount_path.is_some() => {
                        AppMode::Prompt(Prompt::ConfirmSmbDisconnect { share })
                    }
                    _ => AppMode::Network(view),
                }
            }
            _ if hotkeys.network_forget.matches(key) => {
                if self.config.behavior.read_only {
                    self.set_notice("Read-only mode: remembered shares cannot be changed");
                    return AppMode::Network(view);
                }
                match view.shares.get(view.selected).cloned() {
                    Some(share) if share.saved => {
                        AppMode::Prompt(Prompt::ConfirmSmbForget { share })
                    }
                    _ => AppMode::Network(view),
                }
            }
            _ => AppMode::Network(view),
        }
    }

    pub(crate) fn network_open(&mut self, view: NetworkView) -> AppMode {
        let Some(share) = view.shares.get(view.selected).cloned() else {
            return AppMode::Network(view);
        };
        if let Some(path) = share.mount_path.filter(|path| path.is_dir()) {
            self.go_to(path);
            return AppMode::Browser;
        }
        if self.config.behavior.read_only {
            self.set_notice("Read-only mode: network connections are disabled");
            return AppMode::Network(view);
        }
        if share.saved {
            let Some(username) = share.username.clone() else {
                return AppMode::Prompt(Prompt::SmbUsername {
                    address: share.address,
                    input: String::new(),
                    cursor: 0,
                    error: None,
                });
            };
            self.start_network_action(NetworkAction::Connect(ConnectRequest {
                address: share.address,
                auth: NetworkAuth::Saved {
                    username,
                    domain: share.domain.unwrap_or_default(),
                },
            }));
            AppMode::NetworkProgress
        } else {
            AppMode::Prompt(Prompt::SmbUsername {
                address: share.address,
                input: String::new(),
                cursor: 0,
                error: None,
            })
        }
    }

    pub(crate) fn open_network(&mut self) -> AppMode {
        if !self.network_environment.samba_tools_available() {
            return AppMode::Prompt(Prompt::SmbMessage {
                title: "Network shares unavailable".into(),
                body: "Network Shares cannot start because the required desktop network support is unavailable. Install gio and the GVFS Samba backend, then try again.".into(),
                return_to_network: false,
            });
        }
        self.start_network_refresh(None);
        AppMode::Network(NetworkView {
            shares: Vec::new(),
            selected: 0,
        })
    }

    pub(crate) fn start_network_refresh(&mut self, selected_uri: Option<String>) {
        if self.network_refresh.is_some() {
            return;
        }
        let environment = self.network_environment.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || match network::discover_local(&environment) {
            Ok(shares) => {
                if sender
                    .send(NetworkRefreshUpdate::Snapshot {
                        result: Ok(shares),
                        finished: false,
                        secret_storage: None,
                    })
                    .is_err()
                {
                    return;
                }
                let secret_storage = network::secret_service_available(&environment);
                if sender
                    .send(NetworkRefreshUpdate::Snapshot {
                        result: network::discover_local(&environment),
                        finished: false,
                        secret_storage: Some(secret_storage),
                    })
                    .is_err()
                {
                    return;
                }
                let _ = sender.send(NetworkRefreshUpdate::Snapshot {
                    result: network::discover(&environment),
                    finished: true,
                    secret_storage: None,
                });
            }
            Err(error) => {
                let _ = sender.send(NetworkRefreshUpdate::Snapshot {
                    result: Err(error),
                    finished: true,
                    secret_storage: None,
                });
            }
        });
        self.network_refreshing = true;
        self.network_refresh = Some(RunningNetworkRefresh {
            receiver,
            selected_uri,
        });
    }

    pub(crate) fn start_network_action(&mut self, action: NetworkAction) {
        if self.network_operation.is_some() {
            return;
        }
        let environment = self.network_environment.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(network::perform(action, &environment));
        });
        self.network_operation = Some(RunningNetworkOperation { receiver });
    }

    pub(crate) fn network_shares_available(&self) -> bool {
        self.network_environment.samba_tools_available()
    }
}
