//! zg-api 独立可执行：`zg-api [--port 8786] [--offline] [--no-browser]`

fn main() {
    let mut port: u16 = 8786;
    let mut offline = false;
    let mut no_browser = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => {
                if let Some(v) = it.next() {
                    port = v.parse().unwrap_or(8786);
                }
            }
            "--offline" => offline = true,
            "--no-browser" => no_browser = true,
            "-h" | "--help" => {
                println!("用法: zg-api [--port 8786] [--offline] [--no-browser]");
                return;
            }
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    if let Err(e) = zg_api::run(port, offline, no_browser) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
