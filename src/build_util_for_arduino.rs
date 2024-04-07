#[macro_use]
extern crate lazy_static;    

pub fn main_entry() {
    if let Some(info) = arduino_cli_util::BUILD_PROPERTIES.as_ref() {
        let c = compile_bindgen::CompileFactory::new(info);

        let out_lib_dir = info.default_archive_dir();
        c.compile(Some(out_lib_dir));

        #[cfg(feature = "native_bindgen")]
        {
            let bind = compile_bindgen::BindgenFactory::new(info);
            bind.generate_bindings();

        }

    }

    // compile();
    // generate_bindings();
}

#[cfg(feature = "prettify_bindgen")]
#[doc(hidden)]
mod clang_x {
    use clang::*;
    use std::path::PathBuf;

    /// update builder's allowlist. limit the allow scope to the  top items in the bindgen file itself.
    /// not include the contents imported by `#include`
    pub fn update_bindgen_allowlist(
        header: &PathBuf,
        clang_args: &Vec<String>,
        allow_item: &mut Vec<String>,
        allow_type: &mut Vec<String>,
        allow_func: &mut Vec<String>,
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

        // Parse a source file into a translation unit
        let tu = index
            .parser(header)
            .arguments(&clang_args)
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

        *allow_item = vars_defines_collect;

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
        *allow_type = types_collect;

        //3
        let functions_collect: Vec<String> = tu
            .get_entity()
            .get_children()
            .to_owned()
            .into_iter()
            .filter(|e| e.is_in_main_file() && e.get_name().is_some() && is_function(&e.get_kind()))
            .map(|x| x.get_name().unwrap())
            .collect();
        *allow_func = functions_collect;
    }
}

mod compile_bindgen {

    use super::arduino_cli_util;
    #[cfg(feature = "prettify_bindgen")]
    use super::clang_x;
    use std::{
        io::Write,
        path::{Path, PathBuf},
        process::Stdio,
    };
    use tempfile::tempdir;

    /// A builder for creating a [`bindgen::Builder`].
    #[cfg(feature = "native_bindgen")]
    #[derive(Clone, Debug)]
    #[must_use]
    pub struct BindgenFactory<'a> {
        info: &'a arduino_cli_util::Info,
        pub clang_args: Vec<String>,
        pub cpp: bool,
    }

    #[cfg(feature = "native_bindgen")]
    impl<'a> BindgenFactory<'a> {
        pub fn new(info: &'a arduino_cli_util::Info) -> Self {
            let mut clang_args = Vec::<String>::new();
            let mut incs = Vec::<String>::new();

            let mut cpp = true;
            let mut pat = info.get_pat("recipe.cpp.o.pattern");
            if pat.is_none() {
                pat = info.get_pat("recipe.c.o.pattern");
                cpp = false;
            }

            if let Some(p) = pat {
                if let Some(gcc_inc) = output_gcc_sysheader_dirs(p.cmd.as_str(), true) {
                    incs.extend(gcc_inc);
                }
                clang_args.extend(
                    p.flags
                        .iter()
                        .filter(|s| s.starts_with("-D") || s.contains("-mmcu=") )
                        .map(String::from)
                        .collect::<Vec<_>>(),
                );
                incs.extend(p.inc_dirs);
            }
            incs.extend(info.core_incs());
            incs.extend(info.get_external_libraries_path());

            clang_args.extend(incs.iter().map(|t| format!("-I{}", t)).collect::<Vec<_>>());

            Self {
                info,
                clang_args,
                cpp,
            }
        }

        pub fn generate_bindings(&self) {
            let mut header_files = Vec::<PathBuf>::new();
            for folder in self.info.get_external_libraries_path() {
                let lib_headers = files_in_folder(folder.as_str(), "*.h");
                header_files.extend(lib_headers)
            }

            /////////////////////////
            println!("cargo:warning=: begin to generate external lib bindings");

            let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
            let out_file = out_path.join("arduino_sdk_bindings.rs");

            let mut file = std::fs::File::create(out_file).expect("?");

            let mulit_lines = r#"
        extern "C" {
            /// init() is Arduino framework provided function to initialize the board.
            /// down-stream app would need to call it in rust as well before we start using any Arduino sdk library
            pub fn init();
            pub fn user_init();
        }
                    "#;
            let _ = file.write_all(mulit_lines.as_bytes());

            let builder = self.create_base_builder();
            // generate each header in seperate mod, mod name is the header name
            for header_file in &header_files {
                match self.bindgen_a_file(&builder, header_file, &out_path) {
                    Ok(s) =>    file.write_all(s.as_bytes()).expect("Cou"),
                    Err(e)  =>   println!("cargo:warning=: {:?} binding fail:{:?}",header_file.file_name().unwrap(),e),
                }
            }
        }
        /// the header binding success result will be sotred in out_path dir.
        /// result is the contents that need to appended to the fianl bindings.rs
        fn bindgen_a_file(
            &self,
            builder: &bindgen::Builder,
            header_file: &PathBuf,
            out_path: &PathBuf,
        ) -> Result<String, String> {
            let mut b = builder.clone();
            b = b.header(header_file.to_string_lossy());
            
            // builder.command_line_flags()
            #[cfg(feature = "prettify_bindgen")]
            {
                let (mut allow_item, mut allow_type, mut allow_func) = (
                    Vec::<String>::new(),
                    Vec::<String>::new(),
                    Vec::<String>::new(),
                );
                clang_x::update_bindgen_allowlist(
                    header_file,
                    &self.clang_args,
                    &mut allow_item,
                    &mut allow_type,
                    &mut allow_func,
                );
                for i in allow_item {
                    b = b.allowlist_item(i);
                }
                for i in allow_type {
                    b = b.allowlist_type(i);
                }
                for i in allow_func {
                    b = b.allowlist_function(i);
                }
            }

            let _command_line_flags = &b.command_line_flags();
            // generate each header in seperate mod, mod name is the header name
            let header_file_stem = header_file.file_stem().unwrap();
            let mut header_name = header_file_stem.to_string_lossy().to_ascii_lowercase();
            header_name.retain(|c| c != ' ');
            header_name = header_name.replace(".", "_");

            match b.generate(){
                Ok(bindings) =>{
                    let dest_name = format!("{ }.rs", header_file_stem.to_str().unwrap());
                    let dest = out_path.join(&dest_name);

                    if bindings.write_to_file(&dest).is_ok() {
                        cargo_fmt_file(dest);
                        let x = format!(
                            "pub mod {}{{\n  include!(concat!(env!(\"OUT_DIR\"),\"/{}\"));\n}}\n",
                            header_name,
                            dest_name.as_str(),
                        );
                        return Ok(x);
                    } else {
                        return Err("writing file fail".to_string());
                    }
                }
                Err(e) => {
                    return Err(format!("{}",e));
                    // return Err(format!("{}. cmd:{} ",e,command_line_flags.join(" ").as_str()));
                }
            }
        }

        /// Create a [`bindgen::Builder`] with these settings.
        fn create_base_builder(&self) -> bindgen::Builder {
            let mut builder = bindgen::Builder::default()
                .use_core()
                .layout_tests(false)
                .formatter(bindgen::Formatter::None)
                .derive_default(true)
                .clang_arg("-D__bindgen")
                // Include directories provided by the build system
                // should be first on the search path (before sysroot includes),
                // or else libc's <dirent.h> does not correctly override sysroot's <dirent.h>
                .clang_args(&self.clang_args)
                // .clang_args(sysroot_args)
                .clang_args(&["-x", if self.cpp { "c++" } else { "c" }]);

            let fqbn = self.info.get_fqbn();
            let temp = fqbn.splitn(4, ":").collect::<Vec<_>>();
            let arch = temp[1];
            if arch == "avr" {
                builder = builder
                    .ctypes_prefix("crate::rust_ctypes")
                    .size_t_is_usize(false);
            }

            // log::debug!(
            //     "Bindgen builder factory flags: {:?}",
            //     builder.command_line_flags()
            // );

            builder
        }
    }

    /// A builder for creating a [`cc::Build`].
    #[derive(Clone, Debug)]
    #[must_use]
    pub struct CompileFactory<'a> {
        info: &'a arduino_cli_util::Info,
    }

    impl<'a> CompileFactory<'a> {
        pub fn new(info: &'a arduino_cli_util::Info) -> Self {
            Self { info }
        }

        pub fn compile(&self, out_lib_dir: Option<PathBuf>) {
            self.prebuild();
            self.prelink();

            println!("cargo:rerun-if-env-changed=ARDUINO_SDK_CONFIG");

            const CORE_NAME: &str = "arduino_core";
            const EXTERNAL_NAME: &str = "arduino_external";

            //if it is not called in build script, use tempdir
            let out_dir_env = std::env::var("OUT_DIR");
            let obj_out_dir = if out_dir_env.is_ok() {
                None
            } else {
                Some(tempdir().unwrap())
            };

            let out_lib_dir = if let Some(p) = out_lib_dir {
                p
            } else {
                self.info.default_archive_dir()
            };

            let static_core_lib_path = out_lib_dir.join(format!("lib{}.a", CORE_NAME));

            if !static_core_lib_path.exists() {
                let core_srcs =
                    self.compile_core_(&obj_out_dir, &Some(out_lib_dir.to_owned()), CORE_NAME);

                if core_srcs.len() > 0 {
                    let core_lib_check_path =
                        out_lib_dir.join(format!("lib{}.a_srcs.txt", CORE_NAME));
                    if let Ok(mut file) = std::fs::File::create(core_lib_check_path) {
                        let _ = file.write_all(
                            core_srcs
                                .iter()
                                .map(|p| {
                                    let p2: PathBuf =
                                        p.iter().skip_while(|s| *s != "packages").collect();
                                    p2
                                })
                                .map(|s| s.to_string_lossy().to_string())
                                .collect::<Vec<_>>()
                                .join("\n")
                                .as_bytes(),
                        );
                    }
                }
            }
            println!("cargo:rustc-link-search={}", out_lib_dir.to_string_lossy());
            // if static_core_lib_path.exists() {
            println!("cargo:rustc-link-lib=static={}", CORE_NAME);
            // }

            let external_lib_path = out_lib_dir.join(format!("lib{}.a", EXTERNAL_NAME));
            let external_lib_check_path =
                out_lib_dir.join(format!("lib{}.a_srcs.txt", EXTERNAL_NAME));
            let external_srcs =
                self.compile_external_(&obj_out_dir, &Some(out_lib_dir), EXTERNAL_NAME);
            if external_lib_path.exists() {
                println!("cargo:rustc-link-lib=static={}", EXTERNAL_NAME);
            }

            if external_srcs.len() > 0 {
                if let Ok(mut file) = std::fs::File::create(external_lib_check_path) {
                    let _ = file.write_all(
                        external_srcs
                            .iter()
                            .map(|p| {
                                let p2: PathBuf =
                                    p.iter().skip_while(|s| *s != "libraries").collect();
                                p2
                            })
                            .map(|s| s.to_string_lossy().to_string())
                            .collect::<Vec<_>>()
                            .join("\n")
                            .as_bytes(),
                    );
                }
            }
            // #[cfg(esp8266_esp8266)]
            self.external_link();


        }
        fn external_link(&self) {

            if let Some(pat) = self.info.get_pat("recipe.c.combine.pattern") {

                let path_removable = |s: &str| s.contains("{") || s.contains("}");
                let lib_removable = |s: &str| {
                    let t = s.trim();
                    t == "m" || t == "gcc"// || t == "c"
                };

                for lp in pat
                    .flags
                    .iter()
                    .filter_map(|s| s.trim().strip_prefix("-L"))
                    .filter(|s| path_removable(s) == false)
                    .map(|s| PathBuf::from(s))
                    .collect::<Vec<_>>()
                {
                    println!("cargo:rustc-link-search={}", lp.to_string_lossy());
                }

                for lib in pat
                    .flags
                    .iter()
                    .filter_map(|s| s.trim().strip_prefix("-l"))
                   .filter(|s| lib_removable(s) == false)
                    .collect::<Vec<_>>()
                {
                    println!("cargo:rustc-link-lib={}", lib);
                }

            }
        }

        /// compile and got objects. include core and core iteself libraries.
        /// suggest in build script, set obj_out_dir/lib_out_dir to NONE, then it will be automaticaly setted to OUT_DIR env
        fn compile_core_<P1: AsRef<Path>, P2: AsRef<Path>>(
            &self,
            obj_out_dir: &Option<P1>,
            lib_out_dir: &Option<P2>,
            name: &str,
        ) -> Vec<PathBuf> {
            let mut out_objects = Vec::<PathBuf>::new();
            let mut srcs = Vec::<PathBuf>::new();

            // try_compile_intermediates
            let mut builder = cc::Build::new();
            if let Some(p) = obj_out_dir {
                builder.out_dir(p);
            }

            //if it is not called in build script, use temp data to avoid cc-rs requirement
            if std::env::var("OUT_DIR").is_err() {
                let fqbn = self.info.get_fqbn();
                let x = fqbn.splitn(4, ":").collect::<Vec<_>>();
                let (_packager, arch, _boardid) = (x[0], x[1], x[2]);

                builder
                    .target(arch)
                    .opt_level_str("s")
                    .host("x86_64-pc-windows-msvc");
            }

            for p in self.info.core_incs() {
                builder.include(p);
            }

            if let Some(p) = self.info.get_pat(arduino_cli_util::PRIVATE_CORE_DEDICATED) {
                p.inc_dirs.iter().for_each(|i| {
                    builder.include(i);
                });
                p.flags.iter().for_each(|i| {
                    builder.asm_flag(i);
                    builder.flag(i);
                });
            }

            //s
            if let (Some(p), files) = (
                self.info.get_pat("recipe.S.o.pattern"),
                self.core_project_files("*.S"),
            ) {
                srcs.extend(files.to_owned());
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
            if let (Some(p), files) = (
                self.info.get_pat("recipe.c.o.pattern"),
                self.core_project_files("*.c"),
            ) {
                srcs.extend(files.to_owned());
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
                self.info.get_pat("recipe.cpp.o.pattern"),
                self.core_project_files("*.cpp"),
            ) {
                srcs.extend(files.to_owned());
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
            if out_objects.len() > 0 {
                let ar_cmd = self.info.get_ar_cmd().unwrap();
                builder.archiver(ar_cmd.as_str());
                if let Some(out) = lib_out_dir {
                    if !Path::new(out.as_ref()).exists() {
                        let _=std::fs::create_dir_all(out.as_ref());
                    }
                    builder.out_dir(out);
                }
                out_objects.iter().for_each(|t| {
                    builder.object(t);
                });
                builder.compile(name);
            }
            srcs.sort();
            srcs
        }
        /// platform's itself  core + variant + libraries

        /// compile external libraries ,that located in user directory (sketchbook).
        /// suggest in build script, set obj_out_dir/lib_out_dir to NONE, then it will be automaticaly setted to OUT_DIR env
        fn compile_external_<P1: AsRef<Path>, P2: AsRef<Path>>(
            &self,
            obj_out_dir: &Option<P1>,
            lib_out_dir: &Option<P2>,
            name: &str,
        ) -> Vec<PathBuf> {
            let mut builder = cc::Build::new();

            for p in self.info.core_incs() {
                builder.include(p);
            }
            for p in self.info.get_external_libraries_path() {
                builder.include(p);
            }
            if let Some(p) = obj_out_dir {
                builder.out_dir(p);
            }

            //if it is not called in build script, use temp data to avoid cc-rs requirement
            if std::env::var("OUT_DIR").is_err() {
                let fqbn = self.info.get_fqbn();
                let x = fqbn.splitn(4, ":").collect::<Vec<_>>();
                let (_packager, arch, _boardid) = (x[0], x[1], x[2]);

                builder
                    .target(arch)
                    .opt_level_str("s")
                    .host("x86_64-pc-windows-msvc");
            }

            let mut out_objects = Vec::<PathBuf>::new();
            let mut srcs = Vec::<PathBuf>::new();

            //c
            if let (Some(p), files) = (
                self.info.get_pat("recipe.c.o.pattern"),
                self.external_libraries_project_files("*.c"),
            ) {
                srcs.extend(files.to_owned());
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
                self.info.get_pat("recipe.cpp.o.pattern"),
                self.external_libraries_project_files("*.cpp"),
            ) {
                srcs.extend(files.to_owned());
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
                let ar_cmd = self.info.get_ar_cmd().unwrap();
                builder.archiver(ar_cmd.as_str());
                if let Some(out) = lib_out_dir {
                    builder.out_dir(out);
                }
                out_objects.iter().for_each(|t| {
                    builder.object(t);
                });
                builder.compile(name);
            }
            srcs.sort();
            srcs
        }

        fn core_project_files(&self, patten: &str) -> Vec<PathBuf> {
            let mut result = Vec::<PathBuf>::new();

            if let Some(core_path) = self.info.get_var("build.core.path") {
                result = files_in_folder(core_path.as_str(), patten);
            }
            if let Some(p) = self.info.get_var("build.variant.path") {
                let s = files_in_folder(p.as_str(), patten);
                result.extend(s);
            }

            let pattern = format!("**/{}", patten);
            for library in self.info.get_arduino_libraries_path() {
                let lib_sources = files_in_folder(library.as_str(), &pattern);
                result.extend(lib_sources);
            }

            result
        }

        fn external_libraries_project_files(&self, patten: &str) -> Vec<PathBuf> {
            let mut result = Vec::<PathBuf>::new();

            let pattern = format!("**/{}", patten);
            for l in self.info.get_external_libraries_path() {
                let lib_sources = files_in_folder(l.as_str(), &pattern);
                result.extend(lib_sources);
            }
            result
        }


        fn prebuild(&self){
            for mut cmd in self.get_hooks_cmds("prebuild"){
                cmd.spawn().expect("fail");
            }
//         "recipe.hooks.prebuild.2.pattern": RecipePattern {
        }
        fn prelink(&self){
            for mut cmd in self.get_hooks_cmds("linking.prelink"){
                cmd.spawn().expect("fail");
            }
            // "recipe.hooks.linking.prelink.1.pattern"
        }

        // recipe.hooks.XXXXXX.NUMBER.pattern
        fn get_hooks_cmds(&self, name:&str)->Vec<std::process::Command>{

            let pari = [
            ("{build.project_name}",env!("CARGO_PKG_NAME").to_string()),
            ("{build.source.path}",Path::new(env!("CARGO_MANIFEST_DIR")).join("src").to_string_lossy().to_string() ),
            ("{build.path}",std::env::var("OUT_DIR").unwrap()),
            ];

            let prefix = format!("recipe.hooks.{}.",name);

            let mut result = vec![];
            for (k,_v) in &self.info.orig_properties{
                let k = k.as_str().trim();
                if let Some(suf) = k.strip_prefix(&prefix){
                    if let Some(num) = suf.strip_suffix(".pattern"){
                        result.push(num);
                    }
                }
            }
            result.sort_by(|a, b| a.partial_cmp(b).unwrap());

            let mut cmds  = vec![];
            for num in result{
                let patstr=format!("{}{}.pattern",&prefix,&num);
                if let Some(mut pat) =self.info.get_pat(patstr.as_str()){
                    let mut cmd = std::process::Command::new(pat.cmd);
                    pat.flags.iter_mut().for_each(|x| {
                        for (s,d) in &pari{
                            *x = x.replace(s, d.as_str());
                        }
                    } );
                    cmd.args(pat.flags);
                    cmds.push(cmd);
                }
            }
            cmds
//         "recipe.hooks.prebuild.2.pattern": RecipePattern {
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
    /// output c/c++ system header dirs
    fn output_gcc_sysheader_dirs(gcc_cmd_str: &str, cpp: bool) -> Option<Vec<String>> {
        let mut cmd = std::process::Command::new(gcc_cmd_str);

        cmd.arg("-x")
            .arg(if cpp { "c++" } else { "c" })
            .arg("-v")
            .arg("-")
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        if let Ok(output) = cmd.output() {
            let stderr = String::from_utf8(output.stderr).unwrap();
            let mut dirs = vec![];
            let (mut begin, mut end) = (false, false);
            for l in stderr.lines() {
                if l.contains("search starts here:") {
                    begin = true;
                }
                if l.contains("End of search list") {
                    end = true;
                }
                if begin == true && end == false {
                    dirs.push(l);
                }
            }
            let dest = dirs
                .iter()
                .filter(|s| PathBuf::from(s.trim()).is_dir() == true)
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>();

                print!("cargo:warning= got gcc inc-dirs: {:?}",&dest);

            return Some(dest);
        }
        None
    }

    fn cargo_fmt_file(file: impl AsRef<Path>) {
        let file = file.as_ref();
        let mut current = std::process::Command::new("rustfmt");

        if current.arg(file).spawn().is_err() {
            let mut stable_cmd = std::process::Command::new("rustup");
            if stable_cmd
                .arg("run")
                .arg("stable")
                .arg("rustfmt")
                .arg(file)
                .spawn()
                .is_err()
            {
                let mut nightly_cmd = std::process::Command::new("rustup");
                if nightly_cmd
                    .arg("run")
                    .arg("stable")
                    .arg("rustfmt")
                    .arg(file)
                    .spawn()
                    .is_err()
                {
                    println!("cargo:warning=: rustfmt not found in the current toolchain, nor in stable or nightly. \
                                The generated bindings will not be properly formatted.");
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn it_works() {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("abc");

            let x = CompileFactory::new(arduino_cli_util::BUILD_PROPERTIES.as_ref().unwrap());
            println!("{:#?}", x.compile_external_(Some(dir)));
        }
        #[test]
        fn it_works1() {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("abc");
            let x = CompileFactory::new(arduino_cli_util::BUILD_PROPERTIES.as_ref().unwrap());

            println!("{:#?}", x);
        }
    }
}

mod arduino_cli_util {
    use serde_yaml;
    use std::{
        collections::{HashMap, VecDeque},
        path::{Path, PathBuf},
        process::Command,
    };
    type KVMap = HashMap<String, String>;
    pub const PRIVATE_CORE_DEDICATED: &str = "_private_core_dedicated";
    #[derive(Debug, Clone)]
    struct DownStreamConfig {
        input: serde_yaml::Value,
    }
    impl DownStreamConfig {
        pub fn new(env_arduino_sys: Option<&str>) -> Self {
            let default = DownStreamConfig {
                input: serde_yaml::from_str::<serde_yaml::Value>(r#"{ "fqbn":"arduino:avr:uno" }"#)
                    .unwrap(),
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
                    let y = x
                        .iter()
                        .map(|s| {
                            let dir = path_root.join(s.trim());
                            let src_dir = dir.join("src");
                            if src_dir.is_dir() {
                                src_dir
                            } else {
                                dir
                            }
                        })
                        .filter(|t| t.is_dir())
                        .collect::<Vec<_>>();
                    return Some(y);
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

    lazy_static! {
        pub static ref BUILD_PROPERTIES: Option<Info> = Info::new();
    }

    #[derive(Debug, Clone)]
    pub struct Info {
        pub orig_properties: KVMap,
        downstream_config: DownStreamConfig,
        pats: HashMap<String, RecipePattern>,
        user: String,
    }

    impl Info {
        pub fn new() -> Option<Self> {
            let mut build_: Option<Info> = None;

            let downstream_config = if let Ok(env_arduino_sys) = std::env::var("ARDUINO_SDK_CONFIG")
            {
                DownStreamConfig::new(Some(env_arduino_sys.as_str()))
            } else {
                DownStreamConfig::new(None)
            };

            if let Some(fqbn) = downstream_config.get_fqbn() {
                // tell the Rust compiler about the fqbn,this allows us to have conditional Rust code
                let x = fqbn.splitn(4, ":").collect::<Vec<_>>();
                let (packager, arch, _boardid) = (x[0], x[1], x[2]);
                println!("cargo:rustc-cfg={}_{}",packager,arch);

                if let Some(orig_properties) = get_build_properties(fqbn) {
                    let pats = get_patterns_(&orig_properties, &downstream_config);
                    if let Some(user) = get_user() {
                        build_.replace(Info {
                            orig_properties,
                            downstream_config,
                            pats,
                            user,
                        });
                    }
                }
            }
            build_
        }
        pub fn get_fqbn(&self) -> String {
            let x = self.downstream_config.get_fqbn().unwrap().to_string();
            x
        }
        pub fn get_ar_cmd(&self) -> Option<String> {
            if let Some(i) = self.get_pat("recipe.ar.pattern") {
                return Some(i.cmd);
            }
            None
        }

        /// var defined in board.txt and platform.txt
        pub fn get_var(&self, key: &str) -> Option<String> {
            return self.orig_properties.get(key.into()).cloned();
        }
        pub fn get_pat(&self, key: &str) -> Option<RecipePattern> {
            return self.pats.get(key.into()).cloned();
        }
        pub fn core_incs(&self) -> Vec<String> {
            let mut result = Vec::<String>::new();
            if let Some(p) = self.get_var("build.core.path") {
                result.push(p.to_owned());
            }
            if let Some(p) = self.get_var("build.variant.path") {
                result.push(p.to_owned());
            }
            for p in self.get_arduino_libraries_path() {
                result.push(p);
            }

            result
        }

        pub fn get_external_libraries_path(&self) -> Vec<String> {
            let mut result = Vec::<PathBuf>::new();

            if let Some(v) = self
                .downstream_config
                .get_external_libraries_path(&Path::new(&self.user).join("libraries"))
            {
                result.extend(v);
            }

            result
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        }

        pub fn get_arduino_libraries_path(&self) -> Vec<String> {
            let mut final_ret = Vec::<String>::new();
            if let Some(p) = self.get_var("runtime.platform.path") {
                let root = Path::new(p.as_str()).join("libraries");
                let mut result = vec![];
                if let Ok(entrys) = get_dir_entries(&root) {
                    for entry in &entrys {
                        if let Ok(t) = &entry.file_type() {
                            if t.is_dir() {
                                let src_dir = entry.path().join("src");
                                if src_dir.is_dir() {
                                    result.push(src_dir);
                                } else {
                                    result.push(entry.path());
                                }
                            }
                        }
                    }
                }
                result
                    .iter()
                    .for_each(|i| final_ret.push(i.to_string_lossy().to_string()));
            }

            final_ret
        }
        /// (relative to CARGO_MANIFEST_DIR path, absolute path)
        pub fn default_archive_dir(&self) -> PathBuf {
            let fqbn = self.downstream_config.get_fqbn().unwrap();
            let x = fqbn.splitn(4, ":").collect::<Vec<_>>();
            let (packager, arch, boardid) = (x[0], x[1], x[2]);

            let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

            let relative_p = Path::new("arduino-lib")
                .join(packager)
                .join(arch)
                .join(self.get_var("version").unwrap())
                .join("cores")
                .join(self.get_var("build.core").unwrap())
                .join(boardid)
                .join(self.get_var("build.variant").unwrap());

            let arch_dir = manifest_dir.join(&relative_p);

            arch_dir
        }
    }

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

    fn get_patterns_(
        build_properties: &KVMap,
        cust: &DownStreamConfig, //
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

        if let Some(c) = cust.get_compile_flags("c") {
            if let Some(v) = x.get_mut(&"recipe.c.o.pattern".to_string()) {
                v.extend(c);
            }
        }
        if let Some(c) = cust.get_compile_flags("cpp") {
            if let Some(v) = x.get_mut(&"recipe.cpp.o.pattern".to_string()) {
                v.extend(c);
            }
        }
        if let Some(c) = cust.get_compile_flags("asm") {
            if let Some(v) = x.get_mut(&"recipe.S.o.pattern".to_string()) {
                v.extend(c);
            }
        }
        if let Some(c) = cust.get_compile_flags("for_core") {
            core_dedicated.replace(RecipePattern::new(PRIVATE_CORE_DEDICATED,"", &c));
        }

        let mut y = x
            .iter_mut()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    RecipePattern::new(k.as_str(),v.pop_front().unwrap().as_str(), &*v),
                )
            })
            .collect::<HashMap<_, _>>();
        if let Some(t) = core_dedicated {
            y.insert(PRIVATE_CORE_DEDICATED.to_string(), t);
        }

        y
    }

    ///get directories.user from arduino-cli.yaml config file
    fn get_user() -> Option<String> {
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

    /// it like split_whitespace, but it enhanced to deal with quoted string
    fn split_quoted_string(input: &str) -> Vec<String> {
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

    #[derive(Debug, Clone, Default)]
    pub struct RecipePattern {
        pub cmd: String,
        pub flags: Vec<String>,
        /// for o.patern and PRIVATE_CORE_DEDICATED
        pub inc_dirs: Vec<String>,
    }

    impl RecipePattern {
        pub fn new(key:&str,cmd: &str, flags: &VecDeque<String>) -> Self {
             match key {
                "recipe.c.o.pattern" | "recipe.cpp.o.pattern" | "recipe.S.o.pattern" | PRIVATE_CORE_DEDICATED=>{
                    let inc = flags
                        .iter()
                        .filter_map(|s| s.strip_prefix("-I").map(String::from))
                        .collect::<Vec<_>>();

                    let others = &flags
                        .iter()
                        .filter(|s| s.starts_with("-I") == false)
                        .map(|s| s.to_owned())
                        .collect::<Vec<_>>();

                    return Self {
                        cmd: cmd.to_string(),
                        flags: others.clone(),
                        inc_dirs: inc.clone(),
                    };
                }
                _ => {}

             }
             return Self {
                cmd: cmd.to_string(),
                flags: flags.iter().map(|s| s.to_owned()).collect::<Vec::<String>>(),
                inc_dirs: Vec::<String>::new(),
            };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::Info;
        #[test]
        fn it_works() {
            let info = Info::new().unwrap();
            println!("{:#?}", info.default_archive_dir());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let fqbn = "arduino:avr:diecimila:cpu=atmega168";
        // let fqbn = "arduino:esp32:nano_nora:USBMode=hwcdc";
    }
}

// https://stackoverflow.com/questions/74791719/where-are-avr-gcc-libraries-stored/74823286#74823286?newreg=5606ba2c93bc47c9bff2848849d3c78a
// avr-gcc -print-file-name=libc.a -mmcu=...
// Finally, this command will print the location (absolue path) of libraries like libc.a, libm.a, libgcc.a or lib<mcu>.a. The location of the library depends on how the compiler was configureed and installed, but also on command line options like -mmcu=
