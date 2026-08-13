use super::*;

impl App {
    /// Returns the directory that browser-local creation and paste actions
    /// should target. Table view has one explicit current directory. Tree view
    /// can focus nested entries without changing `current_dir`, so actions use
    /// the focused directory or the focused entry's parent instead.
    pub(crate) fn browser_action_directory(&self) -> PathBuf {
        if self.browser_view == BrowserView::Table {
            return self.current_dir.clone();
        }
        let Some(entry) = self.selected_entry() else {
            return self.current_dir.clone();
        };
        if entry.kind == EntryKind::Directory {
            entry.path.clone()
        } else {
            entry
                .path
                .parent()
                .filter(|parent| parent.starts_with(&self.current_dir))
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.current_dir.clone())
        }
    }

    pub(crate) fn reveal_browser_directory(&mut self, directory: &Path) {
        if self.browser_view == BrowserView::Tree && directory != self.current_dir {
            self.expanded_directories.insert(directory.to_path_buf());
        }
    }

    pub(crate) fn refresh(&mut self) {
        let same_root = self.loaded_dir == self.current_dir;
        let preferred = if same_root {
            self.selected_entry().map(|entry| entry.path.clone())
        } else {
            self.selector_memory.get(&self.current_dir).cloned()
        };
        if same_root {
            self.remember_selection();
        }
        self.refresh_browser(preferred);
    }

    pub(crate) fn refresh_search_results(&mut self, renamed: Option<(&Path, &Path)>) {
        let Some(view) = &mut self.search_results else {
            return;
        };
        let selected_path = view
            .results
            .get(view.selected)
            .map(|hit| hit.entry.path.clone());
        search::refresh_hits(&mut view.results, renamed);
        let preferred = renamed
            .and_then(|(old, new)| {
                (selected_path.as_deref() == Some(old)).then(|| new.to_path_buf())
            })
            .or(selected_path);
        view.selected = preferred
            .as_ref()
            .and_then(|path| view.results.iter().position(|hit| &hit.entry.path == path))
            .unwrap_or_else(|| view.selected.min(view.results.len().saturating_sub(1)));
        view.selected_path = view
            .results
            .get(view.selected)
            .map(|hit| hit.entry.path.clone());
        self.search_matches = view.results.len();
    }

    pub(crate) fn refresh_browser(&mut self, preferred: Option<PathBuf>) {
        if cfg!(test) {
            match self.browser_view {
                BrowserView::Tree => self.refresh_tree(preferred),
                BrowserView::Table => self.refresh_table(preferred),
            }
        } else {
            self.request_browser_load(preferred);
        }
    }

    pub(crate) fn request_browser_load(&mut self, preferred: Option<PathBuf>) {
        self.browser_generation = self.browser_generation.wrapping_add(1);
        let marked = self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect();
        let request = LoadRequest {
            generation: self.browser_generation,
            root: self.current_dir.clone(),
            view: self.browser_view,
            ui: self.config.ui.clone(),
            expanded: self.expanded_directories.clone(),
            query: None,
            marked,
            preferred,
            fallback_cursor: self.cursor,
        };
        self.entries.clear();
        self.tree_depths.clear();
        self.cursor = 0;
        self.browser_loading = true;
        self.browser_loaded_entries = 0;
        self.browser_load_elapsed = None;
        self.browser_user_navigated = false;
        if let Some(running) = &self.browser_load {
            running.cancel.store(true, Ordering::Relaxed);
            self.pending_browser_load = Some(request);
        } else {
            self.browser_load = Some(browser_loader::spawn(request));
        }
    }

    pub(crate) fn refresh_table(&mut self, preferred: Option<PathBuf>) {
        let marked = self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        match entry::read_directory(
            &self.current_dir,
            self.config.ui.show_hidden,
            self.config.ui.sort,
            self.config.ui.reverse_sort,
            self.config.ui.directories_first,
        ) {
            Ok(entries) => {
                let entries = entries
                    .into_iter()
                    .map(|mut entry| {
                        entry.selected = marked.contains(&entry.path);
                        entry
                    })
                    .collect::<Vec<_>>();
                self.cursor = preferred
                    .as_ref()
                    .or_else(|| self.selector_memory.get(&self.current_dir))
                    .and_then(|path| entries.iter().position(|entry| &entry.path == path))
                    .unwrap_or_else(|| self.cursor.min(entries.len().saturating_sub(1)));
                self.entries = entries;
                self.tree_depths.clear();
                self.loaded_dir = self.current_dir.clone();
            }
            Err(error) => {
                self.entries.clear();
                self.tree_depths.clear();
                self.cursor = 0;
                self.status = error.to_string();
            }
        }
    }

    pub(crate) fn refresh_tree(&mut self, preferred: Option<PathBuf>) {
        let marked = self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.path.clone())
            .collect::<HashSet<_>>();
        match self.read_expanded_tree() {
            Ok((entries, depths, nested_error)) => {
                let (entries, depths): (Vec<_>, Vec<_>) = entries
                    .into_iter()
                    .zip(depths)
                    .map(|(mut entry, depth)| {
                        entry.selected = marked.contains(&entry.path);
                        (entry, depth)
                    })
                    .unzip();
                self.cursor = preferred
                    .as_ref()
                    .or_else(|| self.selector_memory.get(&self.current_dir))
                    .and_then(|path| entries.iter().position(|entry| &entry.path == path))
                    .unwrap_or_else(|| self.cursor.min(entries.len().saturating_sub(1)));
                self.entries = entries;
                self.tree_depths = depths;
                self.loaded_dir = self.current_dir.clone();
                if let Some(error) = nested_error {
                    self.status = error;
                }
            }
            Err(error) => {
                self.entries.clear();
                self.tree_depths.clear();
                self.cursor = 0;
                self.status = error.to_string();
            }
        }
    }

    pub(crate) fn read_expanded_tree(
        &self,
    ) -> crate::error::Result<(Vec<FileEntry>, Vec<usize>, Option<String>)> {
        fn append_directory(
            path: &Path,
            depth: usize,
            config: &Config,
            expanded: &HashSet<PathBuf>,
            entries: &mut Vec<FileEntry>,
            depths: &mut Vec<usize>,
            nested_error: &mut Option<String>,
        ) -> crate::error::Result<()> {
            let children = entry::read_directory(
                path,
                config.ui.show_hidden,
                config.ui.sort,
                config.ui.reverse_sort,
                config.ui.directories_first,
            )?;
            for child in children {
                let recurse = child.kind == EntryKind::Directory && expanded.contains(&child.path);
                let child_path = child.path.clone();
                entries.push(child);
                depths.push(depth);
                if recurse {
                    if let Err(error) = append_directory(
                        &child_path,
                        depth + 1,
                        config,
                        expanded,
                        entries,
                        depths,
                        nested_error,
                    ) {
                        nested_error.get_or_insert_with(|| error.to_string());
                    }
                }
            }
            Ok(())
        }

        let mut entries = Vec::new();
        let mut depths = Vec::new();
        let mut nested_error = None;
        append_directory(
            &self.current_dir,
            0,
            &self.config,
            &self.expanded_directories,
            &mut entries,
            &mut depths,
            &mut nested_error,
        )?;
        Ok((entries, depths, nested_error))
    }

    pub(crate) fn toggle_browser_view(&mut self) {
        let selected = self.selected_entry().map(|entry| entry.path.clone());
        self.remember_selection();
        match self.browser_view {
            BrowserView::Tree => {
                if let Some(parent) = selected.as_deref().and_then(Path::parent) {
                    self.current_dir = parent.to_path_buf();
                }
                self.browser_view = BrowserView::Table;
                self.expanded_directories.clear();
                self.cursor = 0;
                self.refresh_browser(selected);
                self.set_notice("Table view");
            }
            BrowserView::Table => {
                self.browser_view = BrowserView::Tree;
                self.expanded_directories.clear();
                self.cursor = 0;
                self.refresh_browser(selected);
                self.set_notice("Tree view");
            }
        }
    }

    pub(crate) fn remember_selection(&mut self) {
        if let Some(entry) = self.selected_entry() {
            self.selector_memory
                .insert(self.current_dir.clone(), entry.path.clone());
        }
    }

    pub(crate) fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor =
            (self.cursor as isize + delta).clamp(0, self.entries.len() as isize - 1) as usize;
        if self.browser_loading {
            self.browser_user_navigated = true;
        }
    }

    pub(crate) fn open_selected_table(&mut self) -> AppMode {
        let Some(entry) = self.selected_entry() else {
            return AppMode::Browser;
        };
        if entry.path.is_dir() {
            self.go_to(entry.path.clone());
            AppMode::Browser
        } else {
            self.open_external(false)
        }
    }

    pub(crate) fn activate_tree_entry(&mut self) -> AppMode {
        let Some(entry) = self.selected_entry().cloned() else {
            return AppMode::Browser;
        };
        if entry.kind == EntryKind::Directory {
            if !self.expanded_directories.remove(&entry.path) {
                self.expanded_directories.insert(entry.path.clone());
            }
            self.refresh_browser(Some(entry.path));
            AppMode::Browser
        } else {
            self.open_external(false)
        }
    }

    pub(crate) fn tree_right(&mut self) -> AppMode {
        let Some(entry) = self.selected_entry().cloned() else {
            return AppMode::Browser;
        };
        if entry.kind != EntryKind::Directory {
            return self.open_external(false);
        }
        let depth = self.tree_depth(self.cursor);
        if self.expanded_directories.insert(entry.path.clone()) {
            self.refresh_browser(Some(entry.path));
        } else if self
            .tree_depths
            .get(self.cursor + 1)
            .is_some_and(|child_depth| *child_depth > depth)
        {
            self.cursor += 1;
        }
        AppMode::Browser
    }

    pub(crate) fn tree_left(&mut self) {
        let Some(entry) = self.selected_entry().cloned() else {
            self.go_parent();
            return;
        };
        if entry.kind == EntryKind::Directory && self.expanded_directories.remove(&entry.path) {
            self.refresh_browser(Some(entry.path));
            return;
        }
        let depth = self.tree_depth(self.cursor);
        if depth > 0 {
            if let Some(parent_index) = (0..self.cursor)
                .rev()
                .find(|index| self.tree_depth(*index) + 1 == depth)
            {
                self.cursor = parent_index;
            }
        } else {
            self.go_parent();
        }
    }

    pub(crate) fn go_parent(&mut self) {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        let previous_root = self.current_dir.clone();
        self.remember_selection();
        self.current_dir = parent;
        self.expanded_directories.clear();
        self.cursor = 0;
        self.refresh_browser(Some(previous_root));
    }

    pub(crate) fn go_to(&mut self, path: PathBuf) {
        self.remember_selection();
        let path = if path.is_absolute() {
            path
        } else {
            self.current_dir.join(path)
        };
        if path.is_dir() {
            self.current_dir = path.canonicalize().unwrap_or(path);
            self.expanded_directories.clear();
            self.cursor = 0;
            self.refresh_browser(None);
        } else {
            self.status = format!("Not a directory: {}", path.display());
        }
    }

    pub(crate) fn open_search_result(&mut self, path: &Path) {
        if path.is_dir() {
            self.go_to(path.to_path_buf());
            return;
        }
        let Some(parent) = path.parent() else {
            self.status = format!("Cannot open {}", path.display());
            return;
        };
        self.remember_selection();
        self.current_dir = parent.to_path_buf();
        self.expanded_directories.clear();
        self.cursor = 0;
        self.refresh_browser(Some(path.to_path_buf()));
    }

    pub(crate) fn toggle_selection(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.cursor) {
            entry.selected = !entry.selected;
        }
    }

    pub(crate) fn cycle_sort(&mut self) {
        self.config.ui.sort = match self.config.ui.sort {
            SortSetting::Name => SortSetting::Extension,
            SortSetting::Extension => SortSetting::Size,
            SortSetting::Size => SortSetting::Modified,
            SortSetting::Modified => SortSetting::Type,
            SortSetting::Type => SortSetting::Permissions,
            SortSetting::Permissions => SortSetting::Name,
        };
    }

    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        target_paths_from(&self.entries, self.cursor)
    }

    pub(crate) fn mutation_targets(&mut self) -> Option<Vec<PathBuf>> {
        self.mutation_targets_from(self.selected_paths())
    }

    pub(crate) fn mutation_targets_from(&mut self, paths: Vec<PathBuf>) -> Option<Vec<PathBuf>> {
        if self.config.behavior.read_only {
            self.status = "Read-only mode: file operations are disabled".into();
            return None;
        }
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            None
        } else {
            Some(paths)
        }
    }
}
