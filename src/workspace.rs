use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use serde::Deserialize;
use yukic_ast::{CloadDecl, ImportPath, Item, ItemKind, Module, Visibility};
use yukic_lexer::Lexer;
use yukic_parser::Parser;
use yukic_resolve::{find_module_file_for_path, SearchPaths};
use yukic_span::{FileId, Span};
use yukic_target::TargetTriple;

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct LspSection {
    #[serde(default)]
    pub allow_undef: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct AppManifest {
    pub bin: BinSection,
    #[serde(default)]
    pub libs: LibsSection,
    #[serde(default)]
    pub lsp: LspSection,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LibManifest {
    #[serde(alias = "package")]
    pub lib: LibSection,
    #[serde(default)]
    pub libs: LibsSection,
    #[serde(default)]
    pub lsp: LspSection,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct BinSection {
    pub name: String,
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default = "default_includes")]
    pub includes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct LibSection {
    pub name: String,
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default = "default_includes")]
    pub includes: Vec<String>,
}

fn default_includes() -> Vec<String> {
    vec!["stdlib".to_string(), "core".to_string()]
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct LibsSection {
    #[serde(default)]
    pub runtime: HashMap<String, DependencySpec>,
    #[serde(default)]
    pub build: HashMap<String, DependencySpec>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct DependencySpec {
    pub path: Option<PathBuf>,
    pub git: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub link: Option<String>,
    pub checksum: Option<String>,
    #[serde(default, alias = "sublib", alias = "subpath", alias = "lib")]
    pub package: Option<String>,
    #[serde(default)]
    pub reexport: Option<bool>,
    #[serde(default)]
    pub splitnames: Option<bool>,
    #[serde(default)]
    pub interface: Option<PathBuf>,
    #[serde(default)]
    pub prebuilt: Option<PathBuf>,
    #[serde(default)]
    pub archive: Option<PathBuf>,
    #[serde(default)]
    pub shared: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct WorkspaceManifest {
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub members: HashMap<String, WorkspaceMemberPackage>,
    #[serde(default)]
    pub lsp: LspSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct WorkspaceSection {
    pub name: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct WorkspaceMemberPackage {
    #[serde(default, alias = "type", alias = "output")]
    pub kind: String,
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default = "default_includes")]
    pub includes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ExportedSymbol {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub doc: Option<String>,
    pub span: yukic_span::Span,
    pub file_path: PathBuf,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CachedModule {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub module: Module,
    pub exports: Vec<ExportedSymbol>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CachedCHeader {
    pub header_path: PathBuf,
    pub rel_path: String,
    pub namespace: String,
    pub mtime: SystemTime,
    pub items: Vec<Item>,
    pub symbols: BTreeSet<String>,
    pub exports: Vec<ExportedSymbol>,
    pub symbol_locations: HashMap<String, (PathBuf, u32)>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct WorkspaceEnv {
    pub project_root: PathBuf,
    pub search_paths: SearchPaths,
    pub available_libs: Vec<String>,
    pub splitnames: HashMap<String, Vec<String>>,
    pub prebuilt_interfaces: HashMap<String, PathBuf>,
    pub allow_undef: HashSet<String>,
    cache: Arc<RwLock<HashMap<PathBuf, CachedModule>>>,
    header_cache: Arc<RwLock<HashMap<PathBuf, CachedCHeader>>>,
}

impl WorkspaceEnv {
    pub fn for_path(current_file: &Path) -> Self {
        let (project_root, manifest_type) = find_project_root_and_manifest(current_file);
        let work_dir = project_root.join("yuki-work/local");

        // If cache directory is absent, compile project using yukino compiler
        if !work_dir.exists() {
            trigger_project_build(&project_root);
        }

        let cache_base = resolve_cache_base(&project_root);
        let stdlib_dir = cache_base.join("stdlib");
        let core_dir = cache_base.join("core");
        let macrosdk_dir = cache_base.join("macrosdk");

        ensure_unpacked(&stdlib_dir, yukino_builtin::unpack_stdlib_source);
        ensure_unpacked(&core_dir, yukino_builtin::unpack_core_source);
        ensure_unpacked(&macrosdk_dir, yukino_builtin::unpack_macrosdk_source);

        let mut search_paths = SearchPaths::new();
        let mut available_libs = Vec::new();
        let mut splitnames: HashMap<String, Vec<String>> = HashMap::new();
        let mut prebuilt_interfaces: HashMap<String, PathBuf> = HashMap::new();

        let includes = manifest_type.includes();
        if includes.iter().any(|i| i == "stdlib") {
            search_paths.add_lib("std", &stdlib_dir);
            available_libs.push("std".to_string());
        }
        if includes.iter().any(|i| i == "core") {
            search_paths.add_lib("core", &core_dir);
            available_libs.push("core".to_string());
        }
        if includes.iter().any(|i| i == "macrosdk") {
            search_paths.add_lib("macrosdk", &macrosdk_dir);
            available_libs.push("macrosdk".to_string());
        }

        load_dependencies(
            &project_root,
            &manifest_type,
            &mut search_paths,
            &mut available_libs,
            &mut splitnames,
            &mut prebuilt_interfaces,
        );

        available_libs.sort();
        available_libs.dedup();

        let mut allow_undef = HashSet::new();
        for sym in manifest_type.allow_undef() {
            allow_undef.insert(sym);
        }

        let mut curr = project_root.parent();
        while let Some(parent) = curr {
            let ws_file = parent.join("yukispace.toml");
            if ws_file.is_file() {
                if let Ok(text) = fs::read_to_string(&ws_file) {
                    if let Ok(m) = toml::from_str::<WorkspaceManifest>(&text) {
                        for sym in m.lsp.allow_undef {
                            allow_undef.insert(sym);
                        }
                    }
                }
            }
            if parent.join(".git").is_dir() {
                break;
            }
            curr = parent.parent();
        }

        Self {
            project_root,
            search_paths,
            available_libs,
            splitnames,
            prebuilt_interfaces,
            allow_undef,
            cache: Arc::new(RwLock::new(HashMap::new())),
            header_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn manifest_dir_for(&self, file_path: &Path) -> PathBuf {
        let norm_file = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());
        let norm_root = self
            .project_root
            .canonicalize()
            .unwrap_or_else(|_| self.project_root.clone());
        if norm_file.starts_with(&norm_root) || file_path.as_os_str().is_empty() {
            return self.project_root.clone();
        }
        let (dir, _) = find_project_root_and_manifest(file_path);
        dir
    }

    pub fn list_header_files(&self, current_file: &Path) -> Vec<String> {
        let base = self.manifest_dir_for(current_file);
        let mut out = Vec::new();
        collect_header_files_recursive(&base, &base, &mut out);
        out.sort();
        out.dedup();
        out
    }

    pub fn get_or_load_cheader(&self, rel_path: &str, owner_file: &Path) -> CachedCHeader {
        let manifest_dir = self.manifest_dir_for(owner_file);
        let resolved = if Path::new(rel_path).is_absolute() {
            PathBuf::from(rel_path)
        } else {
            manifest_dir.join(rel_path)
        };
        let cache_key = resolved
            .canonicalize()
            .unwrap_or_else(|_| resolved.clone());
        let mtime = fs::metadata(&resolved)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if let Ok(lock) = self.header_cache.read() {
            if let Some(cached) = lock.get(&cache_key) {
                if cached.mtime == mtime {
                    return cached.clone();
                }
            }
        }

        let pp_cfg = load_cheader_preproc_config(&manifest_dir);
        let namespace = yukic_cheader::header_namespace_for_path(rel_path);
        let result = match yukic_cheader::load_header_from_manifest(
            &manifest_dir,
            rel_path,
            Visibility::Public,
            Span::DUMMY,
            &pp_cfg,
        ) {
            Ok(loaded) => {
                let mut exports = Vec::new();
                for item in &loaded.items {
                    collect_item_exports(item, &cache_key, &mut exports, Visibility::Public);
                }
                let symbol_locations =
                    index_c_symbol_lines(&loaded.tracked_files, &loaded.symbols);
                CachedCHeader {
                    header_path: cache_key.clone(),
                    rel_path: rel_path.to_string(),
                    namespace: loaded.namespace,
                    mtime,
                    items: loaded.items,
                    symbols: loaded.symbols,
                    exports,
                    symbol_locations,
                    error: None,
                }
            }
            Err(err) => CachedCHeader {
                header_path: resolved,
                rel_path: rel_path.to_string(),
                namespace,
                mtime,
                items: Vec::new(),
                symbols: BTreeSet::new(),
                exports: Vec::new(),
                symbol_locations: HashMap::new(),
                error: Some(err.to_string()),
            },
        };

        if let Ok(mut lock) = self.header_cache.write() {
            lock.insert(cache_key, result.clone());
        }
        result
    }

    pub fn collect_cload_headers(
        &self,
        module: &Module,
        current_file: &Path,
    ) -> Vec<(CloadDecl, CachedCHeader)> {
        let mut out = Vec::new();
        for item in &module.items {
            if let ItemKind::Cload(ref decl) = item.kind {
                let hdr = self.get_or_load_cheader(&decl.path, current_file);
                out.push((decl.clone(), hdr));
            }
        }
        out
    }

    pub fn resolve_lib_entry_file(&self, pkg_name: &str) -> Option<PathBuf> {
        let dir = self.search_paths.find_lib(pkg_name)?;

        // 1. Check lib.toml for entry
        if let Ok(text) = fs::read_to_string(dir.join("lib.toml")) {
            if let Ok(manifest) = toml::from_str::<LibManifest>(&text) {
                if let Some(ref entry) = manifest.lib.entry {
                    let cand = dir.join(entry);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
        }
        // 2. Check yuki.toml for entry
        if let Ok(text) = fs::read_to_string(dir.join("yuki.toml")) {
            if let Ok(manifest) = toml::from_str::<AppManifest>(&text) {
                if let Some(ref entry) = manifest.bin.entry {
                    let cand = dir.join(entry);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
        }

        // 3. Check conventional filenames
        let candidates = [
            dir.join("lib.ykn"),
            dir.join(pkg_name).join("lib.ykn"),
            dir.join("core").join("lib.ykn"),
            dir.join("mod.ykn"),
            dir.join("src").join("lib.ykn"),
            dir.join("src").join("main.ykn"),
        ];
        for cand in candidates {
            if cand.is_file() {
                return Some(cand);
            }
        }

        None
    }

    pub fn resolve_import(&self, import_path: &ImportPath, current_file: &Path) -> Option<PathBuf> {
        if let Some(p) = find_module_file_for_path(
            import_path,
            &self.project_root,
            current_file,
            &self.search_paths,
        ) {
            return Some(p);
        }

        // Check splitnames if it's a package import
        if let yukic_ast::PathPrefix::Package(ref id) = import_path.prefix {
            if let Some(splits) = self.splitnames.get(&id.name) {
                for split_pkg in splits {
                    let mut split_import = import_path.clone();
                    split_import.prefix = yukic_ast::PathPrefix::Package(yukic_ast::Ident::new(
                        split_pkg,
                        id.span,
                    ));
                    if let Some(p) = find_module_file_for_path(
                        &split_import,
                        &self.project_root,
                        current_file,
                        &self.search_paths,
                    ) {
                        return Some(p);
                    }
                    if split_import.segments.is_empty() {
                        if let Some(p) = self.resolve_lib_entry_file(split_pkg) {
                            return Some(p);
                        }
                    }
                }
            }
            if import_path.segments.is_empty() {
                if let Some(p) = self.resolve_lib_entry_file(&id.name) {
                    return Some(p);
                }
            }
            let target_name = if import_path.segments.is_empty() {
                id.name.clone()
            } else {
                let segs: Vec<&str> = import_path
                    .segments
                    .iter()
                    .filter_map(|s| match &s.pattern {
                        yukic_ast::ImportPattern::Exact(n) => Some(n.name.as_str()),
                        _ => None,
                    })
                    .collect();
                if segs.is_empty() {
                    id.name.clone()
                } else {
                    format!("{}.{}", id.name, segs.join("."))
                }
            };
            if let Some(p) = self
                .prebuilt_interfaces
                .get(&target_name)
                .or_else(|| self.prebuilt_interfaces.get(&format!("{target_name}.lib")))
                .or_else(|| self.prebuilt_interfaces.get(&format!("{target_name}.mod")))
            {
                return Some(p.clone());
            }
        }

        None
    }

    pub fn get_or_parse_module(&self, path: &Path) -> Option<CachedModule> {
        self.get_or_parse_module_depth(path, 0)
    }

    fn get_or_parse_module_depth(&self, path: &Path, depth: usize) -> Option<CachedModule> {
        let metadata = fs::metadata(path).ok()?;
        let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        {
            let lock = self.cache.read().ok()?;
            if let Some(cached) = lock.get(path) {
                if cached.mtime == mtime {
                    return Some(cached.clone());
                }
            }
        }

        let content = fs::read_to_string(path).ok()?;
        let lexer = Lexer::new(FileId(0), &content);
        let (tokens, _) = lexer.tokenize();
        let mut parser = Parser::with_file_id(tokens, FileId(0));
        let is_ykni = path.extension().and_then(|s| s.to_str()) == Some("ykni");
        let (mut module, _) = if is_ykni {
            parser.parse_interface_module()
        } else {
            parser.parse_module()
        };
        if is_ykni {
            flatten_ykni_namespaces(&mut module.items);
        }

        let mut exports = extract_exports(&module, &content, path);

        // Follow public re-exports (e.g. `public load .io: *` in std/lib.ykn, or `public cload`)
        if depth < 5 {
            for item in &module.items {
                if item.visibility == Visibility::Public {
                    if let ItemKind::Import(ref imp) = item.kind {
                        if let Some(target_file) = self.resolve_import(&imp.path, path) {
                            if target_file != path {
                                if let Some(target_mod) =
                                    self.get_or_parse_module_depth(&target_file, depth + 1)
                                {
                                    for exp in target_mod.exports {
                                        let matched = match &imp.symbols {
                                            None => true,
                                            Some(clause) => clause
                                                .items
                                                .iter()
                                                .any(|it| it.pattern.matches(&exp.name)),
                                        };
                                        if matched && !exports.iter().any(|e| e.name == exp.name) {
                                            exports.push(exp);
                                        }
                                    }
                                }
                            }
                        }
                    } else if let ItemKind::Cload(ref cl) = item.kind {
                        let hdr = self.get_or_load_cheader(&cl.path, path);
                        for exp in hdr.exports {
                            if !exports.iter().any(|e| e.name == exp.name) {
                                exports.push(exp);
                            }
                        }
                    }
                }
            }

            // Merge splitnames exports if this file is the entry point of a package with splitnames
            for (base_pkg, splits) in &self.splitnames {
                let base_entry = self.resolve_lib_entry_file(base_pkg);
                if base_entry.as_deref() == Some(path) {
                    for split_pkg in splits {
                        if let Some(split_entry) = self.resolve_lib_entry_file(split_pkg) {
                            if split_entry != *path {
                                if let Some(split_mod) =
                                    self.get_or_parse_module_depth(&split_entry, depth + 1)
                                {
                                    for exp in split_mod.exports {
                                        if !exports.iter().any(|e| e.name == exp.name) {
                                            exports.push(exp);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let cached = CachedModule {
            path: path.to_path_buf(),
            mtime,
            module,
            exports,
        };

        if let Ok(mut lock) = self.cache.write() {
            lock.insert(path.to_path_buf(), cached.clone());
        }

        Some(cached)
    }

    pub fn list_package_submodules(&self, pkg_name: &str) -> Vec<String> {
        let mut submodules = Vec::new();
        let prefix = format!("{pkg_name}.");
        for ns in self.prebuilt_interfaces.keys() {
            if let Some(rest) = ns.strip_prefix(&prefix) {
                if let Some(first_seg) = rest.split('.').next() {
                    if !first_seg.is_empty() && first_seg != "lib" && first_seg != "mod" {
                        submodules.push(first_seg.to_string());
                    }
                }
            }
        }

        let parts: Vec<&str> = pkg_name.split('.').collect();
        let first = parts[0];
        let base_dir = match self.search_paths.find_lib(first) {
            Some(dir) => {
                let candidate = dir.join(first);
                if candidate.is_dir() {
                    candidate
                } else {
                    dir.to_path_buf()
                }
            }
            None => {
                submodules.sort();
                submodules.dedup();
                return submodules;
            }
        };

        let mut target_dir = base_dir;
        for part in &parts[1..] {
            target_dir = target_dir.join(part);
        }

        if let Ok(entries) = fs::read_dir(target_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if stem.is_empty() || stem == "lib" || stem == "mod" {
                    continue;
                }
                if p.is_file() && p.extension().is_some_and(|e| e == "ykn") {
                    submodules.push(stem.to_string());
                } else if p.is_dir() {
                    submodules.push(stem.to_string());
                }
            }
        }

        submodules.sort();
        submodules.dedup();
        submodules
    }
}

pub fn scan_spes_classes(dir: &Path) -> Vec<String> {
    let mut classes = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|e| e == "spes") {
                if let Ok(content) = fs::read_to_string(&p) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        let name_opt = if let Some(rest) = trimmed.strip_prefix("theme ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("screen ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("dialog ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("component ") {
                            extract_spes_ident(rest)
                        } else if let Some(rest) = trimmed.strip_prefix("use ") {
                            extract_spes_ident(rest)
                        } else {
                            None
                        };
                        if let Some(name) = name_opt {
                            let pascal = to_pascal_case(&name);
                            if !pascal.is_empty() {
                                classes.push(pascal);
                            }
                        }
                    }
                }
            }
        }
    }
    classes.sort();
    classes.dedup();
    classes
}

fn extract_spes_ident(s: &str) -> Option<String> {
    let id: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn to_pascal_case(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap().to_uppercase().to_string();
    format!("{}{}", first, chars.as_str())
}

pub enum LoadedManifest {
    App(AppManifest),
    Lib(LibManifest),
    Workspace(WorkspaceManifest),
    None,
}

impl LoadedManifest {
    pub fn includes(&self) -> Vec<String> {
        match self {
            Self::App(a) => a.bin.includes.clone(),
            Self::Lib(l) => l.lib.includes.clone(),
            Self::Workspace(_) | Self::None => default_includes(),
        }
    }

    pub fn allow_undef(&self) -> Vec<String> {
        match self {
            Self::App(a) => a.lsp.allow_undef.clone(),
            Self::Lib(l) => l.lsp.allow_undef.clone(),
            Self::Workspace(w) => w.lsp.allow_undef.clone(),
            Self::None => Vec::new(),
        }
    }

    pub fn is_present(&self) -> bool {
        !matches!(self, Self::None)
    }
}

pub fn find_project_root_and_manifest(file_path: &Path) -> (PathBuf, LoadedManifest) {
    let mut current = if file_path.is_file() {
        file_path.parent().unwrap_or(Path::new("."))
    } else {
        file_path
    };

    let fallback_root = current.to_path_buf();

    loop {
        let app_file = current.join("yuki.toml");
        if app_file.is_file() {
            if let Ok(text) = fs::read_to_string(&app_file) {
                if let Ok(m) = toml::from_str::<AppManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::App(m));
                }
                if let Ok(m) = toml::from_str::<LibManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::Lib(m));
                }
            }
            return (current.to_path_buf(), LoadedManifest::None);
        }

        let lib_file = current.join("lib.toml");
        if lib_file.is_file() {
            if let Ok(text) = fs::read_to_string(&lib_file) {
                if let Ok(m) = toml::from_str::<LibManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::Lib(m));
                }
            }
            return (current.to_path_buf(), LoadedManifest::None);
        }

        let ws_file = current.join("yukispace.toml");
        if ws_file.is_file() {
            if let Ok(text) = fs::read_to_string(&ws_file) {
                if let Ok(m) = toml::from_str::<WorkspaceManifest>(&text) {
                    return (current.to_path_buf(), LoadedManifest::Workspace(m));
                }
            }
            return (current.to_path_buf(), LoadedManifest::None);
        }

        if current.join(".git").is_dir() {
            return (current.to_path_buf(), LoadedManifest::None);
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    (fallback_root, LoadedManifest::None)
}

fn trigger_project_build(project_root: &Path) {
    let compiler = if which("yukino") {
        "yukino"
    } else if which("yukino-dbg") {
        "yukino-dbg"
    } else {
        return;
    };

    let _ = Command::new(compiler)
        .arg("build")
        .arg("--no-out")
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn which(bin_name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(bin_name).is_file() {
                return true;
            }
        }
    }
    false
}

fn resolve_cache_base(project_root: &Path) -> PathBuf {
    let local = project_root.join("yuki-work/local");
    if local.is_dir() || project_root.join("yuki.toml").is_file() || project_root.join("lib.toml").is_file() {
        let _ = fs::create_dir_all(&local);
        return local;
    }

    let global_cache = std::env::temp_dir().join("yukino_cache");
    let _ = fs::create_dir_all(&global_cache);
    global_cache
}

fn ensure_unpacked<F>(dir: &Path, unpack_fn: F)
where
    F: FnOnce(&Path) -> Result<PathBuf, yukino_builtin::BuiltinError>,
{
    if !dir.exists() || !dir.join(".source_hash").is_file() {
        let _ = unpack_fn(dir);
    }
}

fn load_dependencies(
    project_root: &Path,
    manifest: &LoadedManifest,
    search_paths: &mut SearchPaths,
    available_libs: &mut Vec<String>,
    splitnames: &mut HashMap<String, Vec<String>>,
    prebuilt_interfaces: &mut HashMap<String, PathBuf>,
) {
    let local_deps_dir = project_root.join("yuki-work/local/deps");
    if local_deps_dir.is_dir() {
        search_paths.add_search_dir(&local_deps_dir);
    }

    let deps_dir = project_root.join("yuki-work/deps");
    if deps_dir.is_dir() {
        search_paths.add_search_dir(&deps_dir);
    }

    let offlibs_dir = project_root.join("source/offlibs");
    if offlibs_dir.is_dir() {
        search_paths.add_search_dir(&offlibs_dir);
    }

    // Check ancestor directories for enclosing yukispace.toml
    let mut dir = project_root.to_path_buf();
    while let Some(parent) = dir.parent() {
        let ws_file = parent.join("yukispace.toml");
        if ws_file.is_file() {
            if let Ok(text) = fs::read_to_string(&ws_file) {
                if let Ok(ws) = toml::from_str::<WorkspaceManifest>(&text) {
                    register_workspace_members(&ws, parent, search_paths, available_libs);
                }
            }
            break;
        }
        dir = parent.to_path_buf();
    }

    // Queue-based transitive dependency resolution
    let mut queue: VecDeque<(String, DependencySpec, PathBuf)> = VecDeque::new();

    match manifest {
        LoadedManifest::App(a) => {
            for (alias, spec) in &a.libs.runtime {
                queue.push_back((alias.clone(), spec.clone(), project_root.to_path_buf()));
            }
        }
        LoadedManifest::Lib(l) => {
            available_libs.push(l.lib.name.clone());
            search_paths.add_lib(&l.lib.name, project_root);
            for (alias, spec) in &l.libs.runtime {
                queue.push_back((alias.clone(), spec.clone(), project_root.to_path_buf()));
            }
        }
        LoadedManifest::Workspace(ws) => {
            register_workspace_members(ws, project_root, search_paths, available_libs);
        }
        LoadedManifest::None => {}
    }

    let ws_file = project_root.join("yukispace.toml");
    if ws_file.is_file() && !matches!(manifest, LoadedManifest::Workspace(_)) {
        if let Ok(text) = fs::read_to_string(&ws_file) {
            if let Ok(ws) = toml::from_str::<WorkspaceManifest>(&text) {
                register_workspace_members(&ws, project_root, search_paths, available_libs);
            }
        }
    }

    let mut visited: HashSet<(String, PathBuf)> = HashSet::new();

    while let Some((alias, spec, base_dir)) = queue.pop_front() {
        let has_prebuilt_field = spec.interface.is_some()
            || spec.prebuilt.is_some()
            || spec.archive.is_some()
            || spec.shared.is_some();

        let raw_target_dir = if let Some(ref path) = spec.path {
            let direct = base_dir.join(path);
            direct.canonicalize().unwrap_or(direct)
        } else if has_prebuilt_field {
            base_dir.clone()
        } else {
            let local_dep_path = project_root.join("yuki-work/local/deps").join(&alias);
            let legacy_dep_path = project_root.join("yuki-work/deps").join(&alias);
            if local_dep_path.is_dir() {
                local_dep_path
            } else if legacy_dep_path.is_dir() {
                legacy_dep_path
            } else {
                continue;
            }
        };

        let explicit_iface = spec
            .interface
            .as_ref()
            .or(spec.prebuilt.as_ref())
            .and_then(|raw| {
                let c1 = raw_target_dir.join(raw);
                let c2 = base_dir.join(raw);
                let resolved = if c1.exists() { c1 } else { c2 };
                if resolved.is_file() {
                    Some(resolved)
                } else if resolved.is_dir() {
                    let named = resolved.join(format!("{alias}.ykni"));
                    if named.is_file() {
                        Some(named)
                    } else {
                        fs::read_dir(&resolved).ok().and_then(|es| {
                            es.flatten()
                                .map(|e| e.path())
                                .find(|p| p.is_file() && p.extension().is_some_and(|e| e == "ykni"))
                        })
                    }
                } else {
                    None
                }
            });

        let auto_iface = if explicit_iface.is_none()
            && !raw_target_dir.join("lib.toml").is_file()
            && !raw_target_dir.join("yuki.toml").is_file()
        {
            let named = raw_target_dir.join(format!("{alias}.ykni"));
            named.is_file().then_some(named)
        } else {
            None
        };

        if let Some(iface_path) = explicit_iface.or(auto_iface) {
            available_libs.push(alias.clone());
            register_ykni_namespaces(&alias, &iface_path, prebuilt_interfaces);
            continue;
        }

        let target_dir = if raw_target_dir.join("yukispace.toml").is_file() {
            if let Ok(text) = fs::read_to_string(raw_target_dir.join("yukispace.toml")) {
                if let Ok(ws) = toml::from_str::<WorkspaceManifest>(&text) {
                    register_workspace_members(&ws, &raw_target_dir, search_paths, available_libs);
                }
            }
            let pkg_to_find = spec.package.as_deref().unwrap_or(&alias);
            find_workspace_member_dir(&raw_target_dir, pkg_to_find).unwrap_or(raw_target_dir)
        } else {
            raw_target_dir
        };

        if !visited.insert((alias.clone(), target_dir.clone())) {
            continue;
        }

        available_libs.push(alias.clone());
        search_paths.add_lib(&alias, &target_dir);

        let sub_manifest_path = if target_dir.join("lib.toml").is_file() {
            Some(target_dir.join("lib.toml"))
        } else if target_dir.join("yuki.toml").is_file() {
            Some(target_dir.join("yuki.toml"))
        } else {
            None
        };

        if let Some(sub_path) = sub_manifest_path {
            if let Ok(sub_text) = fs::read_to_string(&sub_path) {
                if let Ok(sub_lib) = toml::from_str::<LibManifest>(&sub_text) {
                    for (child_name, child_spec) in sub_lib.libs.runtime {
                        let is_split = child_spec.splitnames.unwrap_or(false)
                            || child_spec.reexport.unwrap_or(false);
                        if is_split {
                            splitnames
                                .entry(alias.clone())
                                .or_default()
                                .push(child_name.clone());
                        }
                        queue.push_back((child_name, child_spec, target_dir.clone()));
                    }
                } else if let Ok(sub_app) = toml::from_str::<AppManifest>(&sub_text) {
                    for (child_name, child_spec) in sub_app.libs.runtime {
                        let is_split = child_spec.splitnames.unwrap_or(false)
                            || child_spec.reexport.unwrap_or(false);
                        if is_split {
                            splitnames
                                .entry(alias.clone())
                                .or_default()
                                .push(child_name.clone());
                        }
                        queue.push_back((child_name, child_spec, target_dir.clone()));
                    }
                }
            }
        }
    }

    // Transitive closure of splitnames
    let mut changed = true;
    while changed {
        changed = false;
        let keys: Vec<String> = splitnames.keys().cloned().collect();
        for base in keys {
            let current = splitnames.get(&base).cloned().unwrap_or_default();
            for split in current {
                if let Some(nested) = splitnames.get(&split).cloned() {
                    let target_list = splitnames.entry(base.clone()).or_default();
                    for n in nested {
                        if !target_list.contains(&n) {
                            target_list.push(n);
                            changed = true;
                        }
                    }
                }
            }
        }
    }
}

fn flatten_ykni_namespaces(items: &mut Vec<Item>) {
    let mut flat = Vec::new();
    for item in items.drain(..) {
        match item.kind {
            ItemKind::Namespace(ns) => flat.extend(ns.items),
            _ => flat.push(item),
        }
    }
    *items = flat;
}

fn register_ykni_namespaces(
    alias: &str,
    ykni_path: &Path,
    prebuilt_interfaces: &mut HashMap<String, PathBuf>,
) {
    prebuilt_interfaces
        .entry(alias.to_string())
        .or_insert_with(|| ykni_path.to_path_buf());
    let Ok(text) = fs::read_to_string(ykni_path) else {
        return;
    };
    let lexer = Lexer::new(FileId(0), &text);
    let (tokens, _) = lexer.tokenize();
    let mut parser = Parser::with_file_id(tokens, FileId(0));
    let (module, _) = parser.parse_interface_module();
    let mut orig_pkg: Option<String> = None;
    for attr in &module.module_attrs {
        if attr.is("ykni::package") {
            if let yukic_ast::AttributeArgs::Named(pairs) = &attr.args {
                for (k, v) in pairs {
                    if k.name == "name" {
                        if let yukic_ast::ExprKind::Literal(yukic_ast::LiteralValue::String(s)) = &v.kind {
                            orig_pkg = Some(s.clone());
                        }
                    }
                }
            }
        }
    }
    for item in &module.items {
        if let ItemKind::Namespace(ref ns) = item.kind {
            let full_ns = ns
                .path
                .iter()
                .map(|id| id.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if !full_ns.is_empty() {
                prebuilt_interfaces
                    .entry(full_ns.clone())
                    .or_insert_with(|| ykni_path.to_path_buf());
                if let Some(ref pkg) = orig_pkg {
                    if full_ns == *pkg || full_ns == format!("{pkg}.lib") {
                        prebuilt_interfaces.insert(alias.to_string(), ykni_path.to_path_buf());
                    } else if let Some(rest) = full_ns.strip_prefix(&format!("{pkg}.")) {
                        prebuilt_interfaces.insert(
                            format!("{alias}.{rest}"),
                            ykni_path.to_path_buf(),
                        );
                    }
                }
            }
        }
    }
}

fn register_workspace_members(
    ws: &WorkspaceManifest,
    project_root: &Path,
    search_paths: &mut SearchPaths,
    available_libs: &mut Vec<String>,
) {
    for pattern in &ws.workspace.members {
        if pattern.ends_with("/*") {
            let parent = pattern.trim_end_matches("/*");
            let parent_dir = project_root.join(parent);
            if let Ok(entries) = fs::read_dir(&parent_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let pkg_name = read_package_name_from_dir(&path)
                            .or_else(|| path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                        if let Some(name) = pkg_name {
                            if name != "core" && name != "std" && name != "macrosdk" {
                                available_libs.push(name.clone());
                                search_paths.add_lib(&name, &path);
                            }
                        }
                    }
                }
            }
        } else {
            let member_dir = project_root.join(pattern);
            if member_dir.is_dir() {
                let pkg_name = read_package_name_from_dir(&member_dir)
                    .or_else(|| member_dir.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                if let Some(name) = pkg_name {
                    if name != "core" && name != "std" && name != "macrosdk" {
                        available_libs.push(name.clone());
                        search_paths.add_lib(&name, &member_dir);
                    }
                }
            }
        }
    }

    for (name, pkg) in &ws.members {
        available_libs.push(name.clone());
        let entry_dir = if let Some(ref entry) = pkg.entry {
            project_root.join(entry)
        } else {
            project_root.join(name)
        };
        if let Some(parent) = entry_dir.parent() {
            search_paths.add_lib(name, parent);
        } else {
            search_paths.add_lib(name, &entry_dir);
        }
    }
}

fn find_workspace_member_dir(ws_dir: &Path, pkg_name: &str) -> Option<PathBuf> {
    let ws_file = ws_dir.join("yukispace.toml");
    let text = fs::read_to_string(&ws_file).ok()?;
    let ws = toml::from_str::<WorkspaceManifest>(&text).ok()?;
    for pattern in &ws.workspace.members {
        if pattern.ends_with("/*") {
            let parent = pattern.trim_end_matches("/*");
            let parent_dir = ws_dir.join(parent);
            if let Ok(entries) = fs::read_dir(&parent_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let name = read_package_name_from_dir(&path)
                            .or_else(|| path.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                        if let Some(ref n) = name {
                            if n == pkg_name {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        } else {
            let member_dir = ws_dir.join(pattern);
            if member_dir.is_dir() {
                let name = read_package_name_from_dir(&member_dir)
                    .or_else(|| member_dir.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()));
                if let Some(ref n) = name {
                    if n == pkg_name {
                        return Some(member_dir);
                    }
                }
            }
        }
    }
    None
}

fn read_package_name_from_dir(path: &Path) -> Option<String> {
    if let Ok(text) = fs::read_to_string(path.join("lib.toml")) {
        if let Ok(m) = toml::from_str::<LibManifest>(&text) {
            return Some(m.lib.name);
        }
    }
    if let Ok(text) = fs::read_to_string(path.join("yuki.toml")) {
        if let Ok(m) = toml::from_str::<AppManifest>(&text) {
            return Some(m.bin.name);
        }
        if let Ok(m) = toml::from_str::<LibManifest>(&text) {
            return Some(m.lib.name);
        }
    }
    None
}

fn extract_exports(module: &Module, _source: &str, file_path: &Path) -> Vec<ExportedSymbol> {
    let mut exports = Vec::new();
    for item in &module.items {
        if item.visibility != Visibility::Public {
            continue;
        }
        collect_item_exports(item, file_path, &mut exports, item.visibility);
    }
    exports
}

fn collect_header_files_recursive(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if p.is_dir() {
            if matches!(name, "yuki-work" | "target" | ".git" | "node_modules") {
                continue;
            }
            collect_header_files_recursive(root, &p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("h")) {
            if let Ok(rel) = p.strip_prefix(root) {
                let norm = rel
                    .components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .collect::<Vec<_>>()
                    .join("/");
                if !norm.is_empty() {
                    out.push(norm);
                }
            }
        }
    }
}

fn load_cheader_preproc_config(manifest_dir: &Path) -> yukic_cheader::PreprocessConfig {
    let mut include_dirs = Vec::new();
    let mut defines = Vec::new();
    for mf in ["yuki.toml", "lib.toml"] {
        let mf_path = manifest_dir.join(mf);
        if let Ok(text) = fs::read_to_string(&mf_path) {
            if let Ok(val) = text.parse::<toml::Value>() {
                if let Some(c_tbl) = val.get("c").and_then(toml::Value::as_table) {
                    if let Some(arr) = c_tbl.get("include_dirs").and_then(toml::Value::as_array) {
                        for item in arr {
                            if let Some(rel) = item.as_str() {
                                let p = Path::new(rel);
                                include_dirs.push(if p.is_absolute() {
                                    p.to_path_buf()
                                } else {
                                    manifest_dir.join(p)
                                });
                            }
                        }
                    }
                    if let Some(def_tbl) = c_tbl.get("defines").and_then(toml::Value::as_table) {
                        for (k, v) in def_tbl {
                            let s = match v {
                                toml::Value::String(st) => st.clone(),
                                toml::Value::Integer(i) => i.to_string(),
                                toml::Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
                                _ => continue,
                            };
                            defines.push((k.clone(), s));
                        }
                    } else if let Some(def_arr) = c_tbl.get("defines").and_then(toml::Value::as_array)
                    {
                        for entry in def_arr {
                            if let Some(raw) = entry.as_str() {
                                if let Some((k, v)) = raw.split_once('=') {
                                    defines.push((k.trim().to_string(), v.trim().to_string()));
                                } else {
                                    defines.push((raw.trim().to_string(), "1".to_string()));
                                }
                            }
                        }
                    }
                }
                break;
            }
        }
    }
    include_dirs.push(manifest_dir.to_path_buf());
    yukic_cheader::PreprocessConfig {
        target: TargetTriple::host(),
        include_dirs,
        defines,
    }
}

fn index_c_symbol_lines(
    tracked_files: &[PathBuf],
    symbols: &BTreeSet<String>,
) -> HashMap<String, (PathBuf, u32)> {
    let mut locs = HashMap::new();
    for file in tracked_files {
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        for (line_idx, raw_line) in content.lines().enumerate() {
            let trimmed = raw_line.trim();
            if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("/*") {
                continue;
            }
            for token in trimmed.split(|c: char| !c.is_alphanumeric() && c != '_') {
                if !token.is_empty() && symbols.contains(token) && !locs.contains_key(token) {
                    locs.insert(token.to_string(), (file.clone(), line_idx as u32));
                }
            }
        }
    }
    locs
}

fn collect_item_exports(
    item: &Item,
    file_path: &Path,
    exports: &mut Vec<ExportedSymbol>,
    _vis: Visibility,
) {
    match &item.kind {
            ItemKind::Fun(ref f) => {
                let params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| {
                        let mut s = String::new();
                        if p.is_ref && p.is_mut {
                            s.push_str("ref ~");
                        } else if p.is_ref {
                            s.push_str("ref ");
                        }
                        s.push_str(&p.name.name);
                        if let Some(ref ty) = p.ty {
                            s.push_str(": ");
                            s.push_str(&ty.to_string());
                        }
                        if p.is_variadic {
                            s.push_str("...");
                        }
                        s
                    })
                    .collect();
                let ret = if let Some(ref rt) = f.return_type {
                    format!(" -> {rt}")
                } else {
                    String::new()
                };
                exports.push(ExportedSymbol {
                    name: f.name.name.clone(),
                    kind: "function".to_string(),
                    signature: format!("fun {}({}){}", f.name.name, params.join(", "), ret),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Cfun(ref c) => {
                let mut params: Vec<String> = c
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name.name, p.ty))
                    .collect();
                if c.is_variadic {
                    params.push("...".to_string());
                }
                let ret = if let Some(ref rt) = c.return_type {
                    format!(" -> {rt}")
                } else {
                    " -> void".to_string()
                };
                exports.push(ExportedSymbol {
                    name: c.name.name.clone(),
                    kind: "cfun".to_string(),
                    signature: format!("cfun {}({}){};", c.name.name, params.join(", "), ret),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Cvar(ref cv) => {
                exports.push(ExportedSymbol {
                    name: cv.name.name.clone(),
                    kind: "cvar".to_string(),
                    signature: format!("cvar {}: {}", cv.name.name, cv.ty),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Struct(ref s) => {
                let fields: Vec<String> = s
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, f.ty))
                    .collect();
                let signature = if fields.is_empty() {
                    format!("struct {}", s.name.name)
                } else {
                    format!("struct {} {{ {} }}", s.name.name, fields.join(", "))
                };
                exports.push(ExportedSymbol {
                    name: s.name.name.clone(),
                    kind: "struct".to_string(),
                    signature,
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Cstruct(ref cs) => {
                let kw = if cs.is_union { "cunion" } else { "cstruct" };
                let fields: Vec<String> = cs
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, f.ty))
                    .collect();
                let signature = if fields.is_empty() {
                    format!("{kw} {}", cs.name.name)
                } else {
                    format!("{kw} {} {{ {} }}", cs.name.name, fields.join(", "))
                };
                exports.push(ExportedSymbol {
                    name: cs.name.name.clone(),
                    kind: kw.to_string(),
                    signature,
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Class(ref c) => {
                let prefix = if c.is_locked { "locked class" } else { "class" };
                exports.push(ExportedSymbol {
                    name: c.name.name.clone(),
                    kind: "class".to_string(),
                    signature: format!("{prefix} {}", c.name.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Enum(ref e) => {
                let variants: Vec<String> = e
                    .variants
                    .iter()
                    .map(|v| match v {
                        yukic_ast::EnumVariant::Simple(id) => id.name.clone(),
                        yukic_ast::EnumVariant::WithValue(id, _) => format!("{} = ...", id.name),
                        yukic_ast::EnumVariant::Adt(id, fields) => {
                            let fld_strs: Vec<String> = fields
                                .iter()
                                .map(|f| format!("{}: {}", f.name.name, f.ty))
                                .collect();
                            format!("{}({})", id.name, fld_strs.join(", "))
                        }
                    })
                    .collect();
                let signature = if variants.is_empty() {
                    format!("enum {}", e.name.name)
                } else {
                    format!("enum {} {{ {} }}", e.name.name, variants.join(", "))
                };
                exports.push(ExportedSymbol {
                    name: e.name.name.clone(),
                    kind: "enum".to_string(),
                    signature,
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Interface(ref i) => {
                exports.push(ExportedSymbol {
                    name: i.name.name.clone(),
                    kind: "interface".to_string(),
                    signature: format!("interface {}", i.name.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Static(ref s) => {
                let ty_str = if let Some(ref ty) = s.ty {
                    format!(": {ty}")
                } else {
                    String::new()
                };
                exports.push(ExportedSymbol {
                    name: s.name.name.clone(),
                    kind: "constant".to_string(),
                    signature: format!("static {}{}", s.name.name, ty_str),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Macro(ref m) => {
                exports.push(ExportedSymbol {
                    name: m.name.name.clone(),
                    kind: "macro".to_string(),
                    signature: format!("define {}!(...)", m.name.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::TypeAlias(ref ta) => {
                exports.push(ExportedSymbol {
                    name: ta.name.name.clone(),
                    kind: "type".to_string(),
                    signature: format!("type {} = {}", ta.name.name, ta.target),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Alias(ref a) => {
                exports.push(ExportedSymbol {
                    name: a.alias.name.clone(),
                    kind: "alias".to_string(),
                    signature: format!("alias as {}", a.alias.name),
                    doc: None,
                    span: item.span,
                    file_path: file_path.to_path_buf(),
                });
            }
            ItemKind::Externc(ref ext) => {
                for ext_item in &ext.items {
                    match ext_item {
                        yukic_ast::ExterncItem::Cfun(ref c) => {
                            let mut params: Vec<String> = c
                                .params
                                .iter()
                                .map(|p| format!("{}: {}", p.name.name, p.ty))
                                .collect();
                            if c.is_variadic {
                                params.push("...".to_string());
                            }
                            let ret = if let Some(ref rt) = c.return_type {
                                format!(" -> {rt}")
                            } else {
                                " -> void".to_string()
                            };
                            exports.push(ExportedSymbol {
                                name: c.name.name.clone(),
                                kind: "cfun".to_string(),
                                signature: format!(
                                    "cfun {}({}){};",
                                    c.name.name,
                                    params.join(", "),
                                    ret
                                ),
                                doc: None,
                                span: c.span,
                                file_path: file_path.to_path_buf(),
                            });
                        }
                        yukic_ast::ExterncItem::Cstruct(ref cs) => {
                            let kw = if cs.is_union { "cunion" } else { "cstruct" };
                            let fields: Vec<String> = cs
                                .fields
                                .iter()
                                .map(|f| format!("{}: {}", f.name.name, f.ty))
                                .collect();
                            let signature = if fields.is_empty() {
                                format!("{kw} {}", cs.name.name)
                            } else {
                                format!("{kw} {} {{ {} }}", cs.name.name, fields.join(", "))
                            };
                            exports.push(ExportedSymbol {
                                name: cs.name.name.clone(),
                                kind: kw.to_string(),
                                signature,
                                doc: None,
                                span: cs.span,
                                file_path: file_path.to_path_buf(),
                            });
                        }
                        yukic_ast::ExterncItem::Cvar(ref cv) => {
                            exports.push(ExportedSymbol {
                                name: cv.name.name.clone(),
                                kind: "cvar".to_string(),
                                signature: format!("cvar {}: {}", cv.name.name, cv.ty),
                                doc: None,
                                span: cv.span,
                                file_path: file_path.to_path_buf(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
}
