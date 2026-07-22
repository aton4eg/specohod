use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
pub struct Args {
    #[arg(short, long)]
    pub manifest: PathBuf,
}
