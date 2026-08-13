use super::*;

impl App {
    pub(crate) fn start_update_check(&mut self) {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(updater::check(env!("CARGO_PKG_VERSION")));
        });
        self.update_check = Some(RunningUpdateCheck { receiver });
    }

    pub(crate) fn start_update(&mut self, version: &str) -> bool {
        let version = version.to_string();
        let executable = match env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.status = format!("Could not locate the installed binary: {error}");
                return false;
            }
        };
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = updater::install(&version, &executable).map(|()| version);
            let _ = sender.send(result);
        });
        self.update = Some(RunningUpdate { receiver });
        true
    }
}
