use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
pub struct Args {
    #[arg(short, long)]
    pub manifest: PathBuf,
}
