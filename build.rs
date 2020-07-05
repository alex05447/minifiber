use cc;

use std::env;

fn main() {
    let target: String = env::var("TARGET").unwrap();
    let is_win_msvc = target.ends_with("windows-msvc");

    if is_win_msvc {
        enum Arch {
            X86,
            X64,
        }

        let arch = match target.split('-').next().unwrap() {
            "x86" => Arch::X86,
            "x86_64" => Arch::X64,
            arch @ _ => {
                panic!("Unsupported architecture: `{}`", arch);
            }
        };

        let mut path = String::from("src/asm/");

        let mut config = cc::Build::new();

        let file_name = match arch {
            Arch::X86 => "get_current_fiber_x86.asm",
            Arch::X64 => "get_current_fiber_x64.asm",
        };

        path.push_str(file_name);

        config.file(path.as_str());

        config.compile("libget_current_fiber.a");
    }
}
