//! `vkd3d-bench`: our vkd3d-proton's memory allocator under D3D12 workloads,
//! with no title running. Each workload prints its profile as it goes, then
//! the allocator's counters over the run (as the overlay's vkd3d view shows
//! them), and exits 1 if a check failed.
//!
//! cargo run -p vkd3d-bench --target x86_64-pc-windows-gnu --release -- streaming --textures 20000 --rounds 40

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use clap::{Parser, Subcommand, ValueEnum};
    use vkd3d_bench::{Own, Pick, Rng};

    #[derive(Parser)]
    struct Cli {
        /// Where d3d12core.dll and d3d12.dll are (default: VICTORY_VKD3D_DIR,
        /// else ~/.local/share/victory/vkd3d).
        #[arg(long)]
        dir: Option<String>,
        /// The workloads' seed.
        #[arg(long, default_value_t = 0x2545_f491_4f6c_dd1d)]
        seed: u64,
        /// Also print the allocator's counters over the run.
        #[arg(long)]
        counters: bool,
        #[command(subcommand)]
        what: What,
    }

    #[derive(Clone, Copy, ValueEnum)]
    enum PickArg {
        Few,
        Scattered,
        Mixed,
    }

    #[derive(Subcommand)]
    enum What {
        /// Textures made, every other let go, as many made again.
        Holes {
            #[arg(long, default_value_t = 96)]
            count: usize,
            #[arg(long, default_value_t = 512)]
            side: u64,
        },
        /// A seeded mix of textures and buffers made and let go.
        Random {
            #[arg(long, default_value_t = 3000)]
            steps: u32,
            #[arg(long, default_value_t = 500)]
            every: u32,
        },
        /// A world loaded at once, then rounds of newly loaded textures.
        Streaming {
            #[arg(long, default_value_t = 3000)]
            textures: usize,
            #[arg(long, default_value_t = 12)]
            rounds: u32,
            #[arg(long, value_enum, default_value = "mixed")]
            pick: PickArg,
            /// Place the textures in card heaps of our own (best fit at
            /// 64 KiB) instead of vkd3d-proton's chunks.
            #[arg(long)]
            placed: bool,
        },
        /// Every workload with its defaults.
        All,
    }

    let cli = Cli::parse();
    let Some(dir) = cli.dir.clone().or_else(vkd3d_bench::default_dir) else {
        eprintln!("no folder for vkd3d-proton: give --dir");
        return std::process::ExitCode::FAILURE;
    };
    let own = match Own::new(&dir) {
        Ok(own) => own,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut rng = Rng::new(cli.seed);
    let streaming = |own: &Own, rng: &mut Rng, textures, rounds, pick: PickArg, placed: bool| {
        let pick = match pick {
            PickArg::Few => Pick::Few,
            PickArg::Scattered => Pick::Scattered,
            PickArg::Mixed => Pick::Mixed,
        };
        vkd3d_bench::streaming(own, rng, textures, rounds, pick, placed.then(vkd3d_bench::Pool::new))
    };
    let runs: Vec<(&str, vkd3d_bench::Report)> = match cli.what {
        What::Holes { count, side } => vec![("holes", vkd3d_bench::holes(&own, count, side))],
        What::Random { steps, every } => vec![("random", vkd3d_bench::random(&own, &mut rng, steps, every))],
        What::Streaming { textures, rounds, pick, placed } => vec![("streaming", streaming(&own, &mut rng, textures, rounds, pick, placed))],
        What::All => vec![
            ("holes", vkd3d_bench::holes(&own, 96, 512)),
            ("random", vkd3d_bench::random(&own, &mut rng, 3000, 500)),
            ("streaming", streaming(&own, &mut rng, 3000, 12, PickArg::Mixed, false)),
            ("streaming, our heaps", streaming(&own, &mut Rng::new(cli.seed), 3000, 12, PickArg::Mixed, true)),
        ],
    };
    let mut ok = true;
    for (name, report) in runs {
        println!("== {name} ({:.2} s)", report.seconds);
        for line in &report.lines {
            println!("{line}");
        }
        if cli.counters {
            for (group, what, v, count, _) in vkd3d_stats::lay_out(&report.end, &report.start, report.seconds) {
                println!("  {group:<42} {what:<40} {v:>10.2} {count:>10.0}");
            }
        }
        for failure in &report.failed {
            println!("FAILED: {failure}");
            ok = false;
        }
    }
    if ok { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("vkd3d-bench runs as a Windows program: --target x86_64-pc-windows-gnu (under Wine)");
}
