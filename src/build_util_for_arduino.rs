pub type KVMap = std::collections::HashMap<String, String>;
pub use cfg::{main_entry, Config};
use serde_yaml::Value;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

const PRIVATE_CORE_DEDICATED: &str = "_private_core_dedicated";

#[derive(Debug, Clone, Default)]
pub struct RecipePattern {
    pub cmd: String,
    pub flags: Vec<String>,
    pub inc_dirs: Vec<String>,
}

impl RecipePattern {
    pub fn new(cmd: &str, flags: &VecDeque<String>) -> Self {
        let inc = flags
            .iter()
            .filter_map(|s| s.strip_prefix("-I").map(String::from))
            .collect::<Vec<_>>();

        let others = &flags
            .iter()
            .filter(|s| s.starts_with("-I") == false)
            .map(|s| s.to_owned())
            .collect::<Vec<_>>();

        Self {
            cmd: cmd.to_string(),
            flags: others.clone(),
            inc_dirs: inc.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct DownStreamConfig {
    input: serde_yaml::Value,
}
impl DownStreamConfig {
    pub fn new(env_arduino_sys: Option<&str>) -> Self {
        let default = DownStreamConfig {
            input: serde_yaml::from_str::<Value>(r#"{ "fqbn":"arduino:avr:uno" }"#).unwrap(),
        };

        if env_arduino_sys.is_none() {
            return default;
        }

        let env_arduino_sys = env_arduino_sys.unwrap();
        let p = Path::new(env_arduino_sys);

        let binding = if let Ok(b) = std::fs::read_to_string(p) {
            b
        } else {
            return default;
        };

        if let Ok(p) = serde_yaml::from_str::<serde_yaml::Value>(binding.as_str()) {
            return DownStreamConfig { input: p };
        }

        default
    }

    pub fn get_fqbn<'a>(&'a self) -> Option<&'a str> {
        if let Some(fqbn) = self.input.get("fqbn") {
            // check valid
            if let Some(x) = fqbn.as_str() {
                if x.splitn(4, ":").collect::<Vec<_>>().len() >= 3 {
                    return Some(x);
                }
            }
        }
        None
    }

    pub fn get_compile_flags<'a>(&'a self, key: &str) -> Option<VecDeque<String>> {
        if let Some(a) = self.input.get("compile_flags") {
            let x = Self::get_strarray(&a, key)
                .map(|v| v.iter().map(|s| s.to_string()).collect::<VecDeque<_>>());

            return x;
        }

        None
    }
    pub fn get_external_libraries_path<'a>(&'a self, path_root: &Path) -> Option<Vec<PathBuf>> {
        if let Some(x) = Self::get_strarray(&self.input, "external_libraries") {
            if x.len() > 0 {
                let (mut y1, y2): (Vec<_>, Vec<_>) = x
                    .iter()
                    .map(|s| (path_root.join(s).join("src"), path_root.join(s)))
                    .unzip();
                y1.extend(y2);
                return Some(y1);
            }
        }
        None
    }

    fn get_strarray<'a>(input: &'a serde_yaml::Value, key: &str) -> Option<Vec<&'a str>> {
        if let Some(e) = input.get(key) {
            if let Some(es) = e.as_sequence() {
                let ret = es.iter().map(|s| s.as_str().unwrap()).collect::<Vec<_>>();

                return Some(ret);
            }
        }
        None
    }
}

mod cfg {
    use super::{board_txt::BoardInfo, DownStreamConfig};
    use bindgen::Bindings;
    use std::process::Command;
    // use serde::{Deserialize, Serialize};
    use std::{
        // collections::hash_set::HashSet,
        // fs,
        io::Write,
        path::{Path, PathBuf},
    };

    pub fn main_entry() {
        let cfg = Config::new().expect("unable to load");

        cfg.compile();
        cfg.generate_bindings();
    }
    #[derive(Debug)]
    pub struct Config {
        // arduino_data: String,
        // arduino_user: String,

        board_info: BoardInfo,
        external_libraries_path: Option<Vec<PathBuf>>,
        arduino_libraries_path: Vec<PathBuf>,
        downstream_cfg_file: Option<PathBuf>,
    }

    impl Config {
        pub fn new() -> Result<Self, String> {
            let mut cfg_from_file: Option<PathBuf> = None;

            let cust = if let Ok(env_arduino_sys) = std::env::var("ARDUINO_SDK_CONFIG") {
                cfg_from_file = Some(Path::new(env_arduino_sys.as_str()).to_owned());
                DownStreamConfig::new(Some(env_arduino_sys.as_str()))
            } else {
                DownStreamConfig::new(None)
            };

            let fqbn = if let Some(x) = cust.get_fqbn() {
                x
            } else {
                return Err(format!("downstream config error"));
            };

            /////////////////
            let arduino_user = if let Some(x) = arduino_cli_config_get_user() {
                x
            } else {
                return Err("can not find arduino-cli command, pls add it to path".to_string());
            };

            let board_info = match BoardInfo::new(fqbn, Some(&cust)) {
                Ok(b) => b,
                Err(e) => return Err(e),
            };

            let external_libraries_path =
                cust.get_external_libraries_path(&Path::new(&arduino_user).join("libraries"));
            /////////////////

            let arduino_libraries_path = board_info.get_arduino_libraries_path();
            Ok(Self {
                // arduino_data,
                // arduino_user,
                board_info,
                external_libraries_path,
                arduino_libraries_path,
                downstream_cfg_file: cfg_from_file,
            })
        }

        /// (relative to CARGO_MANIFEST_DIR path, absolute path)
        pub fn get_archive_dir(&self) -> (PathBuf, PathBuf) {
            let (packager, arch, boardid) = self.board_info.get_packager_arch_boardid();

            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

            let relative_p = Path::new("arduino-lib")
                .join(packager)
                .join(arch)
                .join(self.board_info.get_var("version").unwrap())
                .join("cores")
                .join(self.board_info.get_var("build.core").unwrap())
                .join(boardid)
                .join(self.board_info.get_var("build.variant").unwrap());

            let arch_dir = manifest_dir.join(&relative_p);

            (relative_p, arch_dir)
        }

        pub fn compile(&self) {
            println!("cargo:rerun-if-env-changed=ARDUINO_SDK_CONFIG");

            const CORE_NAME: &str = "arduino_core";
            const EXTERNAL_NAME: &str = "arduino_external";

            let (_, ar_dir) = self.get_archive_dir();
            if !ar_dir.exists() {
                let _ = std::fs::create_dir_all(&ar_dir);
            }
            let out_dir = std::env::var("OUT_DIR").unwrap();

            let static_core_lib_path = ar_dir.join(format!("lib{}.a", CORE_NAME));

            if !static_core_lib_path.exists() {
                let objs = self.compile_core_();
                if let Some(p) = &self.board_info.get_ar_pat() {
                    let mut ar = std::process::Command::new(p.cmd.as_str());
                    ar.arg("rcs").arg(&static_core_lib_path).args(&objs);
                    ar.status().expect("fail to execute");
                }
            }
            if static_core_lib_path.exists() {
                println!("cargo:rustc-link-search={}", ar_dir.to_string_lossy());
                println!("cargo:rustc-link-lib=static={}", CORE_NAME);
            }

            let external_lib_path = Path::new(&out_dir).join(format!("lib{}.a", EXTERNAL_NAME));
            if let Some(objs) = self.compile_external_() {
                if let Some(p) = &self.board_info.get_ar_pat() {
                    let mut ar = std::process::Command::new(p.cmd.as_str());
                    ar.arg("rcs").arg(&external_lib_path).args(&objs);
                    ar.status().expect("fail to execute");
                }
            }
            if external_lib_path.exists() {
                println!("cargo:rustc-link-search={}", out_dir);
                println!("cargo:rustc-link-lib=static={}", EXTERNAL_NAME);
            }
        }

        /// compile and got objects. include core and core iteself libraries
        fn compile_core_(&self) -> Vec<PathBuf> {
            // try_compile_intermediates
            let mut builder = cc::Build::new();
            if let Some(p) = self.board_info.get_var("build.core.path") {
                builder.include(p);
            }
            if let Some(p) = self.board_info.get_var("build.variant.path") {
                builder.include(p);
            }
            for p in &self.arduino_libraries_path {
                builder.include(p);
            }
            if let Some(p) = self.board_info.get_core_dedicated_pat() {
                p.inc_dirs.iter().for_each(|i| {
                    builder.include(i);
                });
                p.flags.iter().for_each(|i| {
                    builder.asm_flag(i);
                    builder.flag(i);
                });
            }
            // builder
            //     .target("avr-atmega328p")
            //     .opt_level_str("s")
            //     .host("x86_64-pc-windows-msvc");
            builder
                .target("esp32")
                .opt_level_str("s")
                .host("x86_64-pc-windows-msvc");

            let mut out_objects = Vec::<PathBuf>::new();
            //s
            if let (Some(p), files) = (
                &self.board_info.get_asm_pat(),
                self.core_project_files("*.S"),
            ) {
                if files.len() > 0 {
                    println!("cargo:warning=: core asm lib not yet built', building now");
                    let mut b = builder.clone();

                    b.compiler(&p.cmd);
                    p.inc_dirs.iter().for_each(|i| {
                        b.include(i);
                    });
                    p.flags.iter().for_each(|i| {
                        b.asm_flag(i);
                    });
                    files.iter().for_each(|i| {
                        b.file(i);
                    });
                    out_objects.extend(b.compile_intermediates());
                }
            }
            //c
            if let (Some(p), files) = (&self.board_info.get_c_pat(), self.core_project_files("*.c"))
            {
                if files.len() > 0 {
                    println!("cargo:warning=: core c lib not yet built', building now");
                    let mut b = builder.clone();

                    b.compiler(&p.cmd);
                    p.inc_dirs.iter().for_each(|i| {
                        b.include(i);
                    });
                    p.flags.iter().for_each(|i| {
                        b.flag(i);
                    });
                    files.iter().for_each(|i| {
                        b.file(i);
                    });
                    out_objects.extend(b.compile_intermediates());
                }
            }

            //cpp
            if let (Some(p), files) = (
                &self.board_info.get_cpp_pat(),
                self.core_project_files("*.cpp"),
            ) {
                if files.len() > 0 {
                    println!("cargo:warning=: core cpp lib not yet built', building now");
                    let mut b = builder.clone();

                    b.compiler(&p.cmd);
                    p.inc_dirs.iter().for_each(|i| {
                        b.include(i);
                    });
                    p.flags.iter().for_each(|i| {
                        b.flag(i);
                    });
                    files.iter().for_each(|i| {
                        b.file(i);
                    });
                    out_objects.extend(b.compile_intermediates());
                }
            }
            out_objects
        }

        /// compile external libraries ,that located in user directory (sketchbook).
        fn compile_external_(&self) -> Option<Vec<PathBuf>> {
            let external_libraries_path = if let Some(x) = &self.external_libraries_path {
                x
            } else {
                return None;
            };

            if let Some(p) = &self.downstream_cfg_file {
                println!("cargo:rerun-if-changed={}", p.to_string_lossy());
            }

            let mut builder = cc::Build::new();
            // println!("cargo:rustc-link-search=native={}", &ar_dir.display());

            if let Some(p) = self.board_info.get_var("build.core.path") {
                builder.include(p);
            }
            if let Some(p) = self.board_info.get_var("build.variant.path") {
                builder.include(p);
            }
            for p in &self.arduino_libraries_path {
                builder.include(p);
            }
            if let Some(pv) = &self.external_libraries_path {
                for p in pv {
                    builder.include(p);
                }
            }
            // builder
            //     .target("avr-atmega328p")
            //     .opt_level_str("s")
            //     .host("x86_64-pc-windows-msvc");
            builder
                .target("esp32")
                .opt_level_str("s")
                .host("x86_64-pc-windows-msvc");
            let mut out_objects = Vec::<PathBuf>::new();

            //c
            if let (Some(p), files) = (
                &self.board_info.get_c_pat(),
                self.external_libraries_project_files(&external_libraries_path, "*.c"),
            ) {
                if files.len() > 0 {
                    println!("cargo:warning=: external c lib not yet built', building now");
                    let mut b = builder.clone();

                    b.compiler(&p.cmd);
                    p.inc_dirs.iter().for_each(|i| {
                        b.include(i);
                    });
                    p.flags.iter().for_each(|i| {
                        b.flag(i);
                    });
                    files.iter().for_each(|i| {
                        println!("cargo:rerun-if-changed={}", i.to_string_lossy());
                        b.file(i);
                    });
                    if let Ok(v) = b.try_compile_intermediates() {
                        out_objects.extend(v);
                    }
                }
            }
            //cpp
            if let (Some(p), files) = (
                &self.board_info.get_cpp_pat(),
                self.external_libraries_project_files(&external_libraries_path, "*.cpp"),
            ) {
                if files.len() > 0 {
                    println!("cargo:warning=: external cpp lib not yet built', building now",);
                    let mut b = builder.clone();

                    b.compiler(&p.cmd);
                    p.inc_dirs.iter().for_each(|i| {
                        b.include(i);
                    });
                    p.flags.iter().for_each(|i| {
                        b.flag(i);
                    });
                    files.iter().for_each(|i| {
                        println!("cargo:rerun-if-changed={}", i.to_string_lossy());
                        b.file(i);
                    });
                    if let Ok(v) = b.try_compile_intermediates() {
                        out_objects.extend(v);
                    }
                }
            }

            if out_objects.len() > 0 {
                return Some(out_objects);
            }
            None
        }

        pub fn core_project_files(&self, patten: &str) -> Vec<PathBuf> {
            let mut result = Vec::<PathBuf>::new();
            if let Some(core_path) = self.board_info.get_var("build.core.path") {
                result = files_in_folder(core_path.as_str(), patten);
            }
            if let Some(p) = self.board_info.get_var("build.variant.path") {
                let s = files_in_folder(p.as_str(), patten);
                result.extend(s);
            }

            let libraries = self.arduino_libraries_path.clone();

            let pattern = format!("**/{}", patten);
            for library in libraries {
                let lib_sources = files_in_folder(library.to_string_lossy().as_ref(), &pattern);
                result.extend(lib_sources);
            }

            result
        }

        pub fn external_libraries_project_files(
            &self,
            external_libraries_path: &Vec<PathBuf>,
            patten: &str,
        ) -> Vec<PathBuf> {
            let mut result = Vec::<PathBuf>::new();

            let pattern = format!("**/{}", patten);
            for library in external_libraries_path {
                let lib_sources = files_in_folder(library.to_string_lossy().as_ref(), &pattern);
                result.extend(lib_sources);
            }

            result
        }

        pub fn generate_bindings(&self) {
            let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
            let out_file = out_path.join("arduino_sdk_bindings.rs");

            let bindgen_headers = if let Some(x) = self.get_bindgen_headers() {
                x
            } else {
                return;
            };

            let mut file = std::fs::File::create(out_file).expect("?");

            let mulit_lines = r#"
extern "C" {
    /// init() is Arduino framework provided function to initialize the board. 
    /// down-stream app would need to call it in rust as well before we start using any Arduino sdk library    
    pub fn init();
}           
            "#;
            let _ = file.write_all(mulit_lines.as_bytes());

            let builder = self.bindgen_configure();
            // generate each header in seperate mod, mod name is the header name
            for header in &bindgen_headers {
                let mut header_name = header
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                header_name.retain(|c| c != ' ');
                header_name = header_name.replace(".", "_");

                let mut b = builder.clone();
                b = b.header(header.to_string_lossy());

                let bindings: Bindings = b.generate().expect("Unable to generate bindings");

                let dest_name = format!("{ }.rs", header.file_stem().unwrap().to_str().unwrap());
                let dest = out_path.join(&dest_name);

                bindings
                    .write_to_file(&dest)
                    .expect("could not write bindings to file");
                file.write_all(
                    // format!(
                    //     "#[path ={:?}]\npub mod {};\n",
                    //     dest.display(),
                    //     header_name
                    // )
                    format!(
                        "pub mod {}{{\n  include!(concat!(env!(\"OUT_DIR\"),\"/{}\"));\n}}\n",
                        header_name,
                        dest_name.as_str(),
                    )
                    .as_bytes(),
                )
                .expect("Cou");
            }
        }

        fn bindgen_configure(&self) -> bindgen::Builder {
            let (_, arch, _) = self.board_info.get_packager_arch_boardid();
            let mut builder = bindgen::Builder::default();

            let mut flags = ["-x", "c++"].map(String::from).to_vec(); //"-std=gnu++11"
            if let Some(p) = &self.board_info.get_cpp_pat() {
                flags.extend(
                    p.flags
                        .iter()
                        // .filter(|s| s.starts_with("-D") || s.starts_with("-I"))
                        .map(String::from)
                        .collect::<Vec<_>>(),
                );
                if arch == "avr" {
                    // cmd is <gcc-home>/bin/avr-g++,  covert to <gcc-home>/avr/include
                    let x = Path::new(&p.cmd);
                    if x.components().count() >= 3 {
                        let y = x
                            .parent()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .join("avr/include")
                            .to_string_lossy()
                            .to_string();
                        flags.push(format!("-I{}", y));
                    }
                }
            }

            if arch == "avr" {
                builder = builder
                    .ctypes_prefix("crate::rust_ctypes")
                    .size_t_is_usize(false);
            }
            builder = builder.clang_args(&flags).use_core().layout_tests(false);

            if let Some(p) = self.board_info.get_var("build.core.path") {
                builder = builder.clang_arg(&format!("-I{}", p));
            }
            if let Some(p) = self.board_info.get_var("build.variant.path") {
                builder = builder.clang_arg(&format!("-I{}", p));
            }
            for p in &self.arduino_libraries_path {
                builder = builder.clang_arg(&format!("-I{}", p.to_string_lossy()));
            }

            #[cfg(feature = "prettify_bindgen")]
            {
                use crate::clang_x;
                if let Some(x) = self.get_bindgen_headers() {
                    clang_x::update_bindgen_allowlist(&mut builder, &x, &flags);
                }
            }

            builder
        }

        fn get_bindgen_headers(&self) -> Option<Vec<PathBuf>> {
            if let Some(x) = &self.external_libraries_path {
                let mut result = vec![];
                for folder in x {
                    let lib_headers = files_in_folder(folder.to_string_lossy().as_ref(), "*.h");
                    result.extend(lib_headers);
                }
                return Some(result);
            }
            None
        }
    }
    fn files_in_folder(folder: &str, pattern: &str) -> Vec<PathBuf> {
        let pat = format!("{}/{}", folder, pattern);
        let mut results = vec![];

        if let Ok(g) = glob::glob(&pat) {
            for f in g.filter_map(Result::ok) {
                if f.ends_with("main.cpp") == false {
                    results.push(f);
                }
            }
        }
        results
    }
    ///get directories.user from arduino-cli.yaml config file
    fn arduino_cli_config_get_user() -> Option<String> {
        if let Ok(output) = Command::new("arduino-cli")
            .arg("config")
            .arg("dump")
            .arg("--format")
            .arg("yaml")
            .output()
        {
            if let Ok(d) = serde_yaml::from_slice::<serde_yaml::Value>(output.stdout.as_slice()) {
                if let Some(dir) = d.get("directories") {
                    // let data = dir.get("data").unwrap().as_str().unwrap();
                    let user = dir.get("user").unwrap().as_str().unwrap();
                    return Some(user.trim().to_string());
                }
            }
        }

        None
    }
}

mod board_txt {
    use super::{DownStreamConfig, KVMap};
    use super::{RecipePattern, PRIVATE_CORE_DEDICATED};

    use std::{
        collections::{HashMap, VecDeque},
        // io::BufRead,
        path::{Path, PathBuf},
    };

    #[derive(Debug, Clone)]
    pub struct BoardInfo {
        //
        pub fqbn: String,

        /// resolved key/value form platform.txt and board.txt.
        pub build_properties: HashMap<String, String>,
        // pub todo: HashMap<String, String>,
        ///
        pub patterns: HashMap<String, RecipePattern>,
    }

    impl BoardInfo {
        pub fn new(fqbn: &str, cust: Option<&DownStreamConfig>) -> Result<Self, String> {
            let build_properties = if let Some(x) = get_build_properties(fqbn) {
                x
            } else {
                return Err(format!(
                    "execute arduino-cli board details -f -b {} fail",
                    fqbn
                ));
            };

            let patterns = get_patterns_(&build_properties, cust);

            return Ok(Self {
                fqbn: fqbn.to_string(),
                build_properties,
                // todo:_tobedo,
                patterns,
            });
        }
        pub fn get_packager_arch_boardid<'a>(&'a self) -> (&'a str, &'a str, &'a str) {
            let x = self.fqbn.splitn(4, ":").collect::<Vec<_>>();

            (x[0], x[1], x[2])
        }

        /// var defined in board.txt and platform.txt
        pub fn get_var(&self, key: &str) -> Option<&String> {
            self.build_properties.get(key)
        }

        pub fn get_arduino_libraries_path(&self) -> Vec<PathBuf> {
            let library_root = Path::new(
                self.build_properties
                    .get(&"runtime.platform.path".to_string())
                    .unwrap(),
            )
            .join("libraries");
            let mut result = vec![];

            if let Ok(entrys) = get_dir_entries(&library_root) {
                for entry in &entrys {
                    if let Ok(t) = &entry.file_type() {
                        if t.is_dir() {
                            result.push(entry.path().join("src"));
                        }
                    }
                }
            }
            result
        }

        pub fn get_core_dedicated_pat<'a>(&'a self) -> Option<&'a RecipePattern> {
            self.patterns.get(PRIVATE_CORE_DEDICATED.into())
        }
        pub fn get_c_pat<'a>(&'a self) -> Option<&'a RecipePattern> {
            self.patterns.get("recipe.c.o.pattern".into())
        }
        pub fn get_cpp_pat<'a>(&'a self) -> Option<&'a RecipePattern> {
            self.patterns.get("recipe.cpp.o.pattern".into())
        }
        pub fn get_asm_pat<'a>(&'a self) -> Option<&'a RecipePattern> {
            self.patterns.get("recipe.S.o.pattern".into())
        }
        pub fn get_ar_pat<'a>(&'a self) -> Option<&'a RecipePattern> {
            self.patterns.get("recipe.ar.pattern".into())
        }
    }

    /////////////////////

    fn get_dir_entries<P: AsRef<Path>>(
        read_dir_path: P,
    ) -> Result<Vec<std::fs::DirEntry>, std::io::Error> {
        let mut dir_entries = vec![];
        for dir_entry in std::fs::read_dir(read_dir_path)? {
            let dir_entry = dir_entry?;
            dir_entries.push(dir_entry);
        }
        Ok(dir_entries)
    }

    /// get installed platform version
    fn get_build_properties(fqbn: &str) -> Option<KVMap> {
        ////////////////////////
        let output = std::process::Command::new("arduino-cli")
            .arg("board")
            .arg("details")
            .arg("-f")
            .arg("-b")
            .arg(fqbn)
            .arg("--format")
            .arg("yaml")
            .output();
        if let Err(_) = output {
            println!("failed to execute process");
            return None;
        }
        let output = output.unwrap();

        if let Ok(o) = serde_yaml::from_slice::<serde_yaml::Value>(output.stdout.as_slice()) {
            if let Some(v) = o.get("buildproperties") {
                if let Some(vec) = v.as_sequence() {
                    let x = vec
                        .iter()
                        .filter_map(|s| s.as_str())
                        .filter_map(|s| s.split_once("="))
                        .map(|(l, r)| (l.trim().to_string(), r.trim().to_string()))
                        .collect::<KVMap>();

                    return Some(x);
                }
            }
        }
        None
    }
    fn get_patterns_(
        build_properties: &KVMap,
        cust: Option<&DownStreamConfig>,
    ) -> HashMap<String, RecipePattern> {
        let is_removeable = |s_t: &str| {
            s_t.starts_with("@")
                || s_t == "{includes}"
                || s_t == "{source_file}"
                || s_t == "{archive_file_path}"
                || s_t == "-o"
                || s_t == "{object_file}"
                || s_t == "-g"
                || s_t == "-flto"
        };

        let mut x = build_properties
            .iter()
            .filter(|(k, _v)| k.starts_with("recipe.") && k.ends_with(".pattern"))
            .map(|(k, v)| {
                let mut vv = VecDeque::from_iter(split_quoted_string(v.as_str()));
                vv.retain(|i| is_removeable(i.as_str()) == false);
                (k, vv)
            })
            .filter(|(_k, v)| v.len() > 0)
            .collect::<HashMap<_, _>>();

        let mut core_dedicated: Option<RecipePattern> = None;
        if let Some(cu) = cust {
            if let Some(c) = cu.get_compile_flags("c") {
                if let Some(v) = x.get_mut(&"recipe.c.o.pattern".to_string()) {
                    v.extend(c);
                }
            }
            if let Some(c) = cu.get_compile_flags("cpp") {
                if let Some(v) = x.get_mut(&"recipe.cpp.o.pattern".to_string()) {
                    v.extend(c);
                }
            }
            if let Some(c) = cu.get_compile_flags("asm") {
                if let Some(v) = x.get_mut(&"recipe.S.o.pattern".to_string()) {
                    v.extend(c);
                }
            }
            if let Some(c) = cu.get_compile_flags("for_core") {
                core_dedicated.replace(RecipePattern::new("", &c));
            }
        }

        let mut y = x
            .iter_mut()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    RecipePattern::new(v.pop_front().unwrap().as_str(), &*v),
                )
            })
            .collect::<HashMap<_, _>>();
        if let Some(t) = core_dedicated {
            y.insert(PRIVATE_CORE_DEDICATED.to_string(), t);
        }

        y
    }

    //////////////////////

    /// it like split_whitespace, but it enhanced to deal with quoted string
    pub fn split_quoted_string(input: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut current_item = String::new();
        let mut inside_quotes = false;
        let mut quote_char = ' ';

        let mut prev_char: Option<char> = None;

        for c in input.chars() {
            match c {
                '"' | '\'' => {
                    if inside_quotes {
                        if c == quote_char {
                            result.push(current_item.trim().to_string());
                            current_item.clear();
                            inside_quotes = false;
                        } else {
                            current_item.push(c);
                        }
                    } else {
                        if prev_char.is_none() || prev_char == Some(' ') {
                            inside_quotes = true;
                            quote_char = c;
                        } else {
                            current_item.push(c);
                        }
                    }
                }
                ' ' => {
                    if inside_quotes == false && current_item.is_empty() == false {
                        result.push(current_item.trim().to_string());
                        current_item.clear();
                    } else {
                        current_item.push(c);
                    }
                }
                _ => current_item.push(c),
            }
            prev_char = Some(c);
        }

        if !current_item.is_empty() {
            result.push(current_item.trim().to_string());
        }
        result.retain(|s| s.len() > 0);

        result
    }
}

#[cfg(feature = "prettify_bindgen")]
#[doc(hidden)]
mod clang_x {
    use clang::*;
    use std::path::PathBuf;

    /// update builder's allowlist. limit the allow scope to the  top items in the bindgen file itself.
    /// not include the contents imported by `#include`
    pub fn update_bindgen_allowlist(
        builder: &mut bindgen::Builder,
        bindgen_file_paths: &Vec<PathBuf>,
        args: &Vec<String>,
    ) {
        let clang = Clang::new().unwrap();
        let index = Index::new(&clang, false, true);

        let is_defines = |x: &EntityKind| x == &EntityKind::MacroDefinition;
        let is_vars = |x: &EntityKind| x == &EntityKind::VarDecl;
        let is_function = |x: &EntityKind| x == &EntityKind::FunctionDecl;
        let is_types = |x: &EntityKind| {
            x == &EntityKind::StructDecl
                || x == &EntityKind::ClassDecl
                || x == &EntityKind::TypedefDecl
        };

        let mut b = builder.clone();
        for f_path in bindgen_file_paths {
            // Parse a source file into a translation unit
            let tu = index
                .parser(f_path)
                .arguments(&args)
                .detailed_preprocessing_record(true)
                .skip_function_bodies(true)
                .parse()
                .unwrap();

            // 1
            let vars_defines_collect: Vec<String> = tu
                .get_entity()
                .get_children()
                .to_owned()
                .into_iter()
                .filter(|e| {
                    let t = e.get_kind();
                    e.is_in_main_file()
                        && e.get_display_name().is_some()
                        && (is_defines(&t) || is_vars(&t))
                })
                .map(|x| x.get_display_name().unwrap())
                .collect();

            for var in vars_defines_collect {
                b = b.allowlist_item(var);
            }

            //2
            let types_collect: Vec<String> = tu
                .get_entity()
                .get_children()
                .to_owned()
                .into_iter()
                .filter(|e| {
                    e.is_in_main_file() && e.get_display_name().is_some() && is_types(&e.get_kind())
                })
                .map(|x| x.get_display_name().unwrap())
                .collect();
            for t in types_collect {
                b = b.allowlist_type(t);
            }

            //3
            let functions_collect: Vec<String> = tu
                .get_entity()
                .get_children()
                .to_owned()
                .into_iter()
                .filter(|e| {
                    e.is_in_main_file() && e.get_name().is_some() && is_function(&e.get_kind())
                })
                .map(|x| x.get_name().unwrap())
                .collect();
            for f in functions_collect {
                b = b.allowlist_function(f);
            }
        }
        *builder = b;
    }
}

#[cfg(test)]
mod tests {
    use super::board_txt::BoardInfo;
    #[test]
    fn it_works() {
        let fqbn = "arduino:avr:diecimila:cpu=atmega168";
        // let fqbn = "arduino:esp32:nano_nora:USBMode=hwcdc";
        let b = BoardInfo::new(fqbn, None);

        println!("{:#?}", b);
    }
}

// https://stackoverflow.com/questions/74791719/where-are-avr-gcc-libraries-stored/74823286#74823286?newreg=5606ba2c93bc47c9bff2848849d3c78a
// avr-gcc -print-file-name=libc.a -mmcu=...
// Finally, this command will print the location (absolue path) of libraries like libc.a, libm.a, libgcc.a or lib<mcu>.a. The location of the library depends on how the compiler was configureed and installed, but also on command line options like -mmcu=
