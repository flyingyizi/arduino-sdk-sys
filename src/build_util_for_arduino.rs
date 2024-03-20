pub type KVMap = std::collections::HashMap<String, String>;
pub use cfg::{main_entry, Config};

use std::path::{Path, PathBuf};

/// represent a board installed in a special platform ver
#[derive(Debug, Clone)]
pub struct BOARDID {
    pub packager: String,
    pub arch: String,
    pub boardid: String,

    pub platform_ver: String,
}

impl BOARDID {
    pub fn new(fqbn: &str, platform_ver: &str) -> Option<Self> {
        // deal fqbn contains items larger than 3
        let x = fqbn.trim().splitn(4, ':').collect::<Vec<_>>();
        if x.len() >= 3 {
            let x = Self {
                packager: x.get(0).unwrap().trim().to_string(),
                arch: x.get(1).unwrap().trim().to_string(),
                boardid: x.get(2).unwrap().trim().to_string(),
                platform_ver: platform_ver.to_string(),
            };
            return Some(x);
        }
        None
    }
    pub fn platform_relative_path(&self) -> PathBuf {
        let platform_relative = Path::new("packages")
            .join(&self.packager)
            .join("hardware")
            .join(&self.arch)
            .join(&self.platform_ver);
        platform_relative
    }
    pub fn fqbn(&self) -> String {
        format!("{}:{}:{}", self.packager, self.arch, self.boardid)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RecipePattern {
    pub cmd: String,
    pub flags: Vec<String>,
    pub inc_dirs: Vec<String>,
}

impl RecipePattern {
    /// from must be: "asm" or "c", or "cpp"
    pub fn merge_downstream_cfg(&mut self, cust: &DownStreamConfig, from: &str) {
        match from {
            "asm" | "c" | "cpp" => {
                if let Some((normal, inc)) = &cust.get_compile_flags(from) {
                    let normal = normal.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                    self.flags.extend(normal);

                    let inc = inc.iter().map(|s| s.to_string()).collect::<Vec<_>>();
                    self.inc_dirs.extend(inc);
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
struct DownStreamConfig {
    input: serde_json::Value,
}
impl DownStreamConfig {
    pub fn new(env_arduino_sys: Option<&str>) -> Self {
        let default = DownStreamConfig {
            input: serde_json::json!({"fqbn":"arduino:avr:uno"}),
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

        if let Ok(p) = serde_json::from_str::<serde_json::Value>(binding.as_str()) {
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

    pub fn get_compile_flags<'a>(&'a self, key: &str) -> Option<(Vec<&'a str>, Vec<&'a str>)> {
        if let Some(a) = self.input.get("compile_flags") {
            let x = Self::get_strarray(&a, key);

            if let Some(x) = x {
                let inc = x
                    .iter()
                    .filter(|i| i.trim_start().starts_with("-I"))
                    .map(|i| i.trim_start().strip_prefix("-I").unwrap().trim_end())
                    .collect::<Vec<_>>();

                let normal = x
                    .iter()
                    .filter(|i| false == i.trim_start().starts_with("-I"))
                    .map(|i| i.trim())
                    .collect::<Vec<_>>();
                return Some((normal, inc));
            }
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

    fn get_strarray<'a>(input: &'a serde_json::Value, key: &str) -> Option<Vec<&'a str>> {
        if let Some(e) = input.get(key) {
            if let Some(es) = e.as_array() {
                let ret = es.iter().map(|s| s.as_str().unwrap()).collect::<Vec<_>>();

                return Some(ret);
            }
        }
        None
    }
}

mod cfg {
    use super::board_txt::BoardInfo;
    use super::{DownStreamConfig, BOARDID};
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
        arduino_user: String,

        board_info: BoardInfo,
        external_libraries_path: Option<Vec<PathBuf>>,
        arduino_libraries_path: Vec<PathBuf>,
        downstream_cfg_file: Option<PathBuf>,
    }

    impl Config {
        pub fn new() -> Result<Self, String> {
            let mut cfg_from_file: Option<PathBuf> = None;

            let cust = if let Ok(env_arduino_sys) = std::env::var("ARDUINO_SYS") {
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
            let (arduino_data, arduino_user) = if let Some(x) = arduino_cli_config_get_data_user() {
                x
            } else {
                return Err("can not find arduino-cli command, pls add it to path".to_string());
            };

            let mut board_info = if let Some(installed) = get_arduino_cli_installed(&fqbn) {
                match BoardInfo::new(arduino_data.as_str(), &installed) {
                    Ok(b) => b,
                    Err(e) => return Err(e),
                }
            } else {
                return Err(format!("fqbn:{} not installed", fqbn));
            };

            // merge downstream config
            if let Some(x) = board_info.c_pattern.as_mut() {
                x.merge_downstream_cfg(&cust, "c");
                x.flags
                    .retain(|s| s.as_str() != "-g" && s.as_str() != "-flto");
            }
            if let Some(x) = board_info.cpp_pattern.as_mut() {
                x.merge_downstream_cfg(&cust, "cpp");
                x.flags
                    .retain(|s| s.as_str() != "-g" && s.as_str() != "-flto");
            }
            if let Some(x) = board_info.s_pattern.as_mut() {
                x.merge_downstream_cfg(&cust, "asm");
                x.flags
                    .retain(|s| s.as_str() != "-g" && s.as_str() != "-flto");
            }
            let external_libraries_path =
                cust.get_external_libraries_path(&Path::new(&arduino_user).join("libraries"));
            /////////////////

            let arduino_libraries_path = board_info.get_arduino_libraries_path(&arduino_data);
            Ok(Self {
                // arduino_data,
                arduino_user,
                board_info,
                external_libraries_path,
                arduino_libraries_path,
                downstream_cfg_file: cfg_from_file,
            })
        }

        /// (relative to CARGO_MANIFEST_DIR path, absolute path)
        pub fn get_archive_dir(&self) -> (PathBuf, PathBuf) {
            let relative_p = self
                .board_info
                .platform_relative_path()
                .join("cores")
                .join(self.board_info.get_var("build.core").unwrap())
                .join(&self.board_info.board.boardid);
            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

            let relative_p = Path::new("arduino-lib").join(relative_p);
            let arch_dir = manifest_dir.join(&relative_p);

            (relative_p, arch_dir)
        }

        pub fn compile(&self) {
            println!("cargo:rerun-if-env-changed=ARDUINO_SYS");

            const CORE_NAME: &str = "arduino_core";
            const EXTERNAL_NAME: &str = "arduino_external";

            let (_, ar_dir) = self.get_archive_dir();
            if !ar_dir.exists() {
                let _ = std::fs::create_dir_all(&ar_dir);
            }

            let static_core_lib_path = ar_dir.join(format!("lib{}.a", CORE_NAME));

            if !static_core_lib_path.exists() {
                let objs = self.compile_core_();
                if let Some(p) = &self.board_info.ar_pattern {
                    let mut ar = std::process::Command::new(p.cmd.as_str());
                    ar.arg("rcs").arg(&static_core_lib_path).args(&objs);
                    ar.status().expect("fail to execute");
                }
            }
            if static_core_lib_path.exists() {
                println!("cargo:rustc-link-search={}", ar_dir.to_string_lossy());
                println!("cargo:rustc-link-lib=static={}", CORE_NAME);
            }

            let out_dir = std::env::var("OUT_DIR").unwrap();
            let external_lib_path = Path::new(&out_dir).join(format!("lib{}.a", EXTERNAL_NAME));
            if let Some(objs) = self.compile_external_() {
                if let Some(p) = &self.board_info.ar_pattern {
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
            // builder
            //     .target("avr-atmega328p")
            //     .opt_level_str("s")
            //     .host("x86_64-pc-windows-msvc");
            // builder
            //     .target("esp32")
            //     .opt_level_str("s")
            //     .host("x86_64-pc-windows-msvc");

            let mut out_objects = Vec::<PathBuf>::new();
            //s
            if let (Some(p), files) = (&self.board_info.s_pattern, self.core_project_files("*.S")) {
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
            if let (Some(p), files) = (&self.board_info.c_pattern, self.core_project_files("*.c")) {
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
                &self.board_info.cpp_pattern,
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

        fn archive_objects(
            ar_cmd: &str,
            dest: &Path,
            objects: &Vec<PathBuf>,
            other_archive: Option<&PathBuf>,
        ) {
            let mut ar = std::process::Command::new(ar_cmd);

            if let Some(p) = other_archive {
                ar.arg("rcsT").arg(dest).args(objects).arg(p);
            } else {
                ar.arg("rcs").arg(dest).args(objects);
            }
            ar.status().expect("failed to execute");
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
            // builder
            //     .target("esp32")
            //     .opt_level_str("s")
            //     .host("x86_64-pc-windows-msvc");
            let mut out_objects = Vec::<PathBuf>::new();

            //c
            if let (Some(p), files) = (
                &self.board_info.c_pattern,
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
                &self.board_info.c_pattern,
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
            let out_file = out_path.join("bindings.rs");

            let bindgen_headers = if let Some(x) = self.get_bindgen_headers() {
                x
            } else {
                return;
            };

            let mut file = std::fs::File::create(out_file).expect("?");

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
            let mut builder = bindgen::Builder::default();

            let mut flags = ["-x", "c++"].map(String::from).to_vec(); //"-std=gnu++11"
            if let Some(p) = &self.board_info.cpp_pattern {
                flags.extend(
                    p.flags
                        .iter()
                        // .filter(|s| s.starts_with("-D") || s.starts_with("-I"))
                        .map(String::from)
                        .collect::<Vec<_>>(),
                );
                if self.board_info.board.arch == "avr" {
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

            if self.board_info.board.arch.as_str() == "avr" {
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
    /// get installed platform version
    fn get_arduino_cli_installed(fqbn: &str) -> Option<BOARDID> {
        ////////////////////////
        let output = Command::new("arduino-cli")
            .arg("board")
            .arg("listall")
            .arg("--format")
            .arg("json")
            .output();
        if let Err(_) = output {
            println!("failed to execute process");
            return None;
        }
        let output = output.unwrap();

        let target_fqbn = serde_json::json!(fqbn);
        if let Ok(o) = serde_json::from_slice::<serde_json::Value>(output.stdout.as_slice()) {
            if let Some(v) = o.get("boards") {
                if let Some(vec) = v.as_array() {
                    for b in vec {
                        if b.get("fqbn") == Some(&target_fqbn) {
                            let t = b.get("platform").unwrap();
                            let ver = t.get("installed").unwrap().as_str().unwrap();
                            return BOARDID::new(fqbn, ver);
                        }
                    }
                }
            }
        }
        None
    }

    ///get(directories.data, directories.user) from arduino-cli.yaml config file
    fn arduino_cli_config_get_data_user() -> Option<(String, String)> {
        if let Ok(output) = Command::new("arduino-cli")
            .arg("config")
            .arg("dump")
            .arg("--format")
            .arg("json")
            .output()
        {
            if let Ok(d) = serde_json::from_slice::<serde_json::Value>(output.stdout.as_slice()) {
                if let Some(dir) = d.get("directories") {
                    let data = dir.get("data").unwrap().as_str().unwrap();
                    let user = dir.get("user").unwrap().as_str().unwrap();
                    return Some((data.trim().to_string(), user.trim().to_string()));
                }
            }
        }

        None
    }
}

mod board_txt {
    use super::{package_index::PackageIndexJSON, KVMap};
    use super::{RecipePattern, BOARDID};

    use std::path::{Path, PathBuf};

    use std::{
        collections::{HashMap, HashSet},
        // io::BufRead,
    };
    type BoardTXT = HashMap<String, KVMap>;

    #[derive(Debug, Clone)]
    pub struct BoardInfo {
        //
        pub board: BOARDID,

        /// resolved key/value form platform.txt and board.txt.
        pub kv: HashMap<String, String>,
        // pub todo: HashMap<String, String>,
        ///
        pub s_pattern: Option<RecipePattern>,
        pub cpp_pattern: Option<RecipePattern>,
        pub c_pattern: Option<RecipePattern>,
        pub ar_pattern: Option<RecipePattern>,
    }

    impl BoardInfo {
        pub fn new(data_path: &str, install_board: &BOARDID) -> Result<Self, String> {
            let data_path = Path::new(data_path.trim());
            let platform_path = data_path.join(install_board.platform_relative_path());

            let boardtxt_path = &platform_path.join("boards.txt");
            let platformtxt_path = &platform_path.join("platform.txt");

            let platform_kv: KVMap;
            let board_kv: KVMap;

            match parse_boardtxt(boardtxt_path) {
                Ok(i) => {
                    if let Some(t) = i.get(&install_board.boardid) {
                        board_kv = t.to_owned();
                    } else {
                        return Err(format!("board {} can not find", &install_board.fqbn()));
                    }
                }
                Err(e) => {
                    return Err(e);
                }
            }

            let package_index = PackageIndexJSON::new(data_path, install_board.arch.as_str());

            match std::fs::read_to_string(platformtxt_path) {
                Ok(i) => {
                    let mut orig = i;
                    for (bk, bv) in &board_kv {
                        let (from, to) = (format!("{{{}}}", bk), bv.as_str());
                        orig = orig.replace(from.as_str(), to);
                    }
                    platform_kv = strlines_to_kv(&orig);
                }
                Err(e) => {
                    return Err(e.to_string());
                }
            }

            //before analyze kv,store the orig
            let orig_recipe_c_o_pattern = platform_kv.get("recipe.c.o.pattern".into()).cloned();
            let orig_recipe_S_o_pattern = platform_kv.get("recipe.S.o.pattern".into()).cloned();
            let orig_recipe_cpp_o_pattern = platform_kv.get("recipe.cpp.o.pattern".into()).cloned();
            let orig_recipe_ar_pattern = platform_kv.get("recipe.ar.pattern".into()).cloned();

            let (kv, _tobedo) = analyze_kv(
                data_path,
                &install_board,
                &board_kv,
                &platform_kv,
                &package_index,
            );

            let c_pattern = parse_recipe_xx_patern(&orig_recipe_c_o_pattern, &kv);
            let cpp_pattern = parse_recipe_xx_patern(&orig_recipe_cpp_o_pattern, &kv);
            let s_pattern = parse_recipe_xx_patern(&orig_recipe_S_o_pattern, &kv);
            let ar_pattern = parse_recipe_xx_patern(&orig_recipe_ar_pattern, &kv);

            return Ok(Self {
                board: install_board.clone(),
                kv,
                // todo:_tobedo,
                c_pattern,
                cpp_pattern,
                s_pattern,
                ar_pattern,
            });
        }

        /// var defined in board.txt and platform.txt
        pub fn get_var(&self, key: &str) -> Option<&String> {
            self.kv.get(key)
        }

        /// get relative to arduino-data path
        pub fn platform_relative_path(&self) -> PathBuf {
            self.board.platform_relative_path()
        }
        pub fn get_arduino_libraries_path(&self, arduino_data: &str) -> Vec<PathBuf> {
            let library_root = Path::new(&arduino_data)
                .join(&self.platform_relative_path())
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

    //////////////////////

    /// output board infos . output likes
    ///```text
    /// {  "uno": {"build.core": "arduino",...  }, .... }
    ///```
    ///
    fn parse_boardtxt(boardtxt_path: &Path) -> Result<BoardTXT, String> {
        let mut all: BoardTXT = HashMap::new();

        let binding = if let Ok(b) = std::fs::read_to_string(boardtxt_path) {
            b
        } else {
            return Err(format!("open fail:{:?}", boardtxt_path));
        };

        // collect boardid
        for line in binding.lines() {
            if let Some((first, _)) = line.split_once('=') {
                if let Some(id) = first.trim().strip_suffix(".name") {
                    all.insert(id.to_string(), HashMap::new());
                }
            }
        }

        for line in binding.lines() {
            if let Some((first, second)) = line.split_once('=') {
                let key = first.trim().to_string();
                let value = second.trim().to_string();

                let (b, bk) = key.split_once('.').unwrap();
                if let Some(x) = all.get_mut(b) {
                    x.insert(bk.to_string(), value);
                }
            }
        }

        all.retain(|_k, v| v.is_empty() == false);

        Ok(all)
    }

    /// return (finall, tobedone)
    fn analyze_kv(
        data_path: &Path,
        board: &BOARDID,
        board_kv: &KVMap,
        platform_kv: &KVMap,
        package_index: &PackageIndexJSON,
    ) -> (
        HashMap<String, String>, /*finall*/
        HashMap<String, String>, /*tobedone*/
    ) {
        let mut final_kv = HashMap::<String, String>::new();
        let mut tobe_expand_kv = HashMap::<String, String>::new();

        //internal var
        final_kv.insert("build.arch".to_string(), board.arch.to_ascii_uppercase());
        final_kv.insert("runtime.ide.version".to_string(), "10819".to_string()); //https://github.com/arduino/arduino-cli/issues/725
        final_kv.insert(
            "runtime.platform.path".to_string(),
            data_path
                .join(board.platform_relative_path())
                .to_string_lossy()
                .to_string(),
        );
        tobe_expand_kv.insert(
            "build.core.path".to_string(),
            "{runtime.platform.path}/cores/{build.core}".to_string(),
        );
        tobe_expand_kv.insert(
            "build.variant.path".to_string(),
            "{runtime.platform.path}/variants/{build.variant}".to_string(),
        );

        for (k, v) in platform_kv {
            if v.contains("{") {
                tobe_expand_kv.insert(k.to_owned(), v.to_owned());
            } else {
                final_kv.insert(k.to_owned(), v.to_owned());
            }
        }
        // vars is high prioriy than platform
        for (k, v) in board_kv {
            if v.contains("{") {
                let _ = tobe_expand_kv.insert(k.to_owned(), v.to_owned());
            } else {
                let _ = final_kv.insert(k.to_owned(), v.to_owned());
            }
        }

        //collect all runtime.tools.xxx.path vars
        let mut runtimetools_vars = HashMap::<String, String>::new();
        for (_k, v) in &tobe_expand_kv {
            for s in get_all_refs(v.as_str()) {
                if s.starts_with("runtime.tools.") && s.ends_with(".path") {
                    let _ = runtimetools_vars.insert(s.to_string(), "".to_string());
                }
            }
        }

        for (k, v) in &mut runtimetools_vars {
            if let Some(p) = &package_index.search_runtime_tools(&board, k) {
                *v = data_path.join(p).to_string_lossy().to_string();
            }
        }
        for (k, v) in runtimetools_vars {
            if v != "" {
                final_kv.insert(k, v);
            }
        }

        resolve_kv(&mut final_kv, &mut tobe_expand_kv);

        (final_kv, tobe_expand_kv)
    }

    fn resolve_kv(finally: &mut HashMap<String, String>, todo: &mut HashMap<String, String>) {
        if todo.len() == 0 {
            return;
        }
        let mut str = kv_to_strlines(&todo);
        let str_b = str.clone();

        for var in get_all_refs(str_b.as_str()) {
            if let Some(fina) = finally.get(var) {
                let from = format!("{{{}}}", var);
                str = str.replace(from.as_str(), fina);
            }
        }
        if str == str_b {
            return;
        }

        *todo = strlines_to_kv(&str);

        for (k, v) in &*todo {
            if v.contains("{") == false {
                finally.insert(k.to_owned(), v.to_owned());
            }
        }

        todo.retain(|_k, v| v.as_str().contains("{"));

        return resolve_kv(finally, todo);
    }

    pub fn get_all_refs<'a>(s: &'a str) -> HashSet<&'a str> {
        let mut result = HashSet::<&str>::new();
        let mut last_openbr: Option<usize> = None;

        for (i, c) in s.chars().enumerate() {
            if c == '{' {
                last_openbr = Some(i + 1);
            } else if c == '}' {
                if let Some(b) = last_openbr {
                    result.insert(&s[b..i]);
                    last_openbr = None;
                }
            }
        }

        result
    }

    fn try_replace_vars(s: &str, finally: &HashMap<String, String>) -> Option<String> {
        let mut ret = s.to_string();

        let mut modified = false;
        let o = get_all_refs(s);
        if o.len() > 0 {
            for s in o {
                if let Some(fina) = finally.get(s) {
                    modified = true;
                    let from = format!("{{{}}}", s);
                    ret = ret.replace(from.as_str(), fina);
                }
            }
        }
        if modified {
            return Some(ret);
        }
        None
    }

    /// orig_s:input orignal recipe pattern value part,
    fn parse_recipe_xx_patern(
        orig: &Option<String>,
        kv: &HashMap<String, String>,
    ) -> Option<RecipePattern> {
        if orig.is_none() {
            return None;
        }
        let orig_s = orig.as_ref().unwrap().as_str();

        let mut ret = RecipePattern::default();

        let mut v = split_quoted_string(orig_s.trim());
        ret.cmd = v.get(0).cloned().unwrap();
        v.drain(0..1);

        //remove -o "{object_file}"
        if let Some(index) = v.iter().position(|s| *s == "-o") {
            v.remove(index);
            if index < v.len() {
                v.remove(index);
            }
        }

        for s in v {
            let s_t = s.trim();
            if let Some(left) = s_t.strip_prefix("-I") {
                ret.inc_dirs.push(left.to_string());
            } else if s_t.starts_with("@")
                || s_t == "{includes}"
                || s_t == "{source_file}"
                || s_t == "{archive_file_path}"
                || s_t == "{object_file}"
            {
            } else {
                ret.flags.push(s_t.to_string());
            }
        }

        //1
        if let Some(s) = try_replace_vars(ret.cmd.as_str(), kv) {
            ret.cmd = s;
        }
        //2
        for v in &mut ret.flags {
            if let Some(s) = try_replace_vars(v.as_str(), kv) {
                *v = s;
            }
        }
        let mut t = Vec::<String>::new();
        for s in &ret.flags {
            t.extend(split_quoted_string(s.as_str()));
        }
        ret.flags = t;
        //3
        for v in &mut ret.inc_dirs {
            if let Some(s) = try_replace_vars(v.as_str(), kv) {
                *v = s;
            }
        }

        // if any "-I" flag is in flags, move them to inc_dirs
        ret.flags.iter().for_each(|s| {
            if let Some(left) = s.strip_prefix("-I") {
                ret.inc_dirs.push(left.to_string());
            }
        });
        ret.flags.retain(|s| s.starts_with("-I") == false);

        Some(ret)
    }

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

    pub fn kv_to_strlines(map: &KVMap) -> String {
        let mut ret = String::default();
        for (k, v) in map {
            ret += format!("{}={}\n", k, v).as_str();
        }
        ret
    }

    pub fn strlines_to_kv(str: &String) -> KVMap {
        let mut all: KVMap = HashMap::new();
        for line in str.lines() {
            if let Some((first, second)) = line.split_once('=') {
                let key = first.trim().to_string();
                let value = second.trim().to_string();

                all.insert(key, value);
            }
        }
        all
    }
}

#[doc(hidden)]
mod package_index {
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};

    use super::BOARDID;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct ToolDefJson {
        packager: String,
        name: String,
        version: String,
    }

    impl ToolDefJson {
        /// relative to arduino_data path
        pub fn get_relative_tool_path(&self) -> PathBuf {
            Path::new("packages")
                .join(&self.packager)
                .join("tools")
                .join(&self.name)
                .join(&self.version)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct PlatformJson {
        pub architecture: String,
        pub version: String,
        pub toolsDependencies: Vec<ToolDefJson>,
    }
    impl PlatformJson {
        /// check var whether exist in self. include_ver identify the var whether include version
        ///
        fn check<'a>(&'a self, var: &str, include_ver: bool) -> Option<&'a ToolDefJson> {
            if include_ver {
                if let Some(index) = self
                    .toolsDependencies
                    .iter()
                    .position(|x| format!("{}-{}", x.name, x.version) == var)
                {
                    return self.toolsDependencies.get(index);
                }
            }
            if let Some(index) = self.toolsDependencies.iter().position(|x| x.name == var) {
                return self.toolsDependencies.get(index);
            }

            None
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct PackageJson {
        name: String,
        platforms: Vec<PlatformJson>,
    }

    /// notes: all version related list is sorted in decending order
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct PackageIndexJSON {
        packages: Vec<PackageJson>,
    }

    impl PackageIndexJSON {
        pub fn new(data_path: &Path, architecture_filter: &str) -> Self {
            if let Some(x) = parse_package_index_json(data_path, architecture_filter) {
                return x;
            }
            return Self {
                packages: Vec::<PackageJson>::new(),
            };
        }

        /// runtime_tools_var example: runtime.tools.avr-gcc.path.
        /// result is relative path. e.g. "packages/arduino/tools/arduinoota/1.3.0". data_path join it will be the finall path.
        pub fn search_runtime_tools(
            &self,
            board: &BOARDID,
            runtime_tools_var: &str,
        ) -> Option<PathBuf> {
            let runtime_tools_var = runtime_tools_var
                .trim()
                .trim_start_matches("runtime.tools.")
                .trim_end_matches(".path");

            let (packager, platform_ver) = (&board.packager, &board.platform_ver);

            //first try, guess the var include version
            if let Some(p) = self.get_package_ref(packager) {
                if let Some(pl) = self.get_platform_ref(packager, platform_ver) {
                    if let Some(td) = pl.check(runtime_tools_var, true) {
                        return Some(td.get_relative_tool_path());
                    }
                }
                //try in same packager
                for pl in &p.platforms {
                    if let Some(td) = pl.check(runtime_tools_var, true) {
                        return Some(td.get_relative_tool_path());
                    }
                }
            }

            // second try, guess the var is not include version
            if let Some(p) = self.get_package_ref(packager) {
                if let Some(pl) = self.get_platform_ref(packager, platform_ver) {
                    if let Some(td) = pl.check(runtime_tools_var, false) {
                        return Some(td.get_relative_tool_path());
                    }
                }
                //try in same packager
                for pl in &p.platforms {
                    if let Some(td) = pl.check(runtime_tools_var, false) {
                        return Some(td.get_relative_tool_path());
                    }
                }
            }
            // try in all packages
            for p in &self.packages {
                for pl in &p.platforms {
                    if let Some(td) = pl.check(runtime_tools_var, true) {
                        return Some(td.get_relative_tool_path());
                    }
                    if let Some(td) = pl.check(runtime_tools_var, false) {
                        return Some(td.get_relative_tool_path());
                    }
                }
            }

            return None;
        }

        fn get_package_ref(&self, packager: &str) -> Option<&PackageJson> {
            if let Some(index) = &self.packages.iter().position(|x| &x.name == packager) {
                return self.packages.get(*index);
            }
            None
        }
        fn get_platform_ref(&self, packager: &str, platform_ver: &str) -> Option<&PlatformJson> {
            if let Some(p) = self.get_package_ref(&packager) {
                if let Some(index) = p.platforms.iter().position(|x| platform_ver == x.version) {
                    return p.platforms.get(index);
                }
            }
            None
        }
    }

    /// parse and store . attention: all platforms and toolsdependencies stored in descending order
    fn parse_package_index_json(
        data_path: &Path,
        architecture_filter: &str,
    ) -> Option<PackageIndexJSON> {
        let package_index_json_path = data_path.join("package_index.json");

        let mut config = if let Ok(cfgstring) = std::fs::read_to_string(package_index_json_path) {
            let config: PackageIndexJSON =
                serde_json::from_str(cfgstring.as_str()).expect("Unable to parse");
            config
        } else {
            return None;
        };

        for package in &mut config.packages {
            package
                .platforms
                .retain(|x| x.architecture.as_str() == architecture_filter);
        }
        config.packages.retain(|x| x.platforms.len() > 0);

        //sort in-descending-order
        for p in &mut config.packages {
            p.platforms
                .sort_by(|a, b| b.version.partial_cmp(&a.version).unwrap());
            for pf in &mut p.platforms {
                pf.toolsDependencies
                    .sort_by(|a, b| b.version.partial_cmp(&a.version).unwrap())
            }
        }

        Some(config)
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
    #[test]
    fn it_works() {}
}


// https://stackoverflow.com/questions/74791719/where-are-avr-gcc-libraries-stored/74823286#74823286?newreg=5606ba2c93bc47c9bff2848849d3c78a
// avr-gcc -print-file-name=libc.a -mmcu=...
// Finally, this command will print the location (absolue path) of libraries like libc.a, libm.a, libgcc.a or lib<mcu>.a. The location of the library depends on how the compiler was configureed and installed, but also on command line options like -mmcu=