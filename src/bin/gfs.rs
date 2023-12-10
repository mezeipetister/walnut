use std::{
    fs::File,
    io::{BufReader, BufWriter, Cursor, Write},
    path::Path,
    time::Instant,
};
use walnut::FS;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    fs_path: String,
    secret: String,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init,
    /// Adds files to myapp
    Add {
        from: String,
        path: String,
        filename: String,
    },
    Remove {
        path: String,
        filename: String,
    },
    Get {
        path: String,
        filename: String,
    },
    Copy {
        from: String,
        to: String,
    },
    Fsinfo,
    Fileinfo {
        path: String,
        filename: String,
    },
    Ls {
        path: String,
    },
    Lsdir,
    Export {
        path: String,
        filename: String,
        out: String,
    },
}

fn main() {
    let path = Path::new("demo/demo.db");

    let cli = Cli::parse();

    // let mut fs = if path.exists() {
    //     fs::FS::new(path, &cli.secret).unwrap()
    // } else {
    //     fs::FS::init(path, &cli.secret).unwrap()
    // };

    match cli.command {
        Commands::Init => init(&cli.fs_path, &cli.secret),
        Commands::Fsinfo => {
            let mut fs = FS::new(&cli.fs_path, &cli.secret).unwrap();
            println!("{:?}", &fs.superblock)
        }
        Commands::Fileinfo { path, filename } => {
            let mut fs = FS::new(&cli.fs_path, &cli.secret).unwrap();
            let inode = fs.get_file_info(&path, &filename).unwrap();
            println!("{:?}", &inode);
        }
        Commands::Ls { path } => {
            let mut fs = FS::new(&cli.fs_path, &cli.secret).unwrap();
            let (dir, _) = fs.find_directory(&path).unwrap();
            dir.files
                .iter()
                .for_each(|f| println!("{0: <20} | inode: {1}", f.0, f.1))
        }
        Commands::Lsdir => {
            let mut fs = FS::new(&cli.fs_path, &cli.secret).unwrap();
            let dirindex = fs.get_directory_index().unwrap();
            dirindex.directories().iter().for_each(|(dir, _index)| {
                println!("{}", dir.to_string_lossy());
            });
        }
        Commands::Add {
            from,
            path,
            filename,
        } => {
            add_file(&cli.fs_path, &cli.secret, &from, &path, &filename);
        }
        Commands::Copy { from, to } => {
            let start = Instant::now();

            let from = File::open(from).unwrap();
            let to = File::create(to).unwrap();

            let mut r = BufReader::new(&from);
            let mut w = BufWriter::new(&to);
            std::io::copy(&mut r, &mut w).unwrap();

            let duration = start.elapsed();
            println!("Time alapsed: {} millisec", duration.as_millis());
        }
        Commands::Remove { path, filename } => {
            remove_file(&cli.fs_path, &cli.secret, &path, &filename);
        }
        Commands::Get { path, filename } => {
            print_file(&cli.fs_path, &cli.secret, &path, &filename);
        }
        Commands::Export {
            path,
            filename,
            out,
        } => export(&cli.fs_path, &cli.secret, &path, &filename, &out),
    }
}

fn add_file(fs_path: &str, secret: &str, file_path: &str, path: &str, file_name: &str) {
    let mut fs = FS::new(fs_path, secret).unwrap();

    let start = Instant::now();

    fs.create_directory(path).unwrap();

    let d = std::fs::File::open(file_path).unwrap();
    let mut data = BufReader::new(&d);

    fs.add_file(path, file_name, &mut data, d.metadata().unwrap().len())
        .unwrap();

    let duration = start.elapsed();
    println!("Time alapsed: {} millisec", duration.as_millis());
}

fn remove_file(fs_path: &str, secret: &str, path: &str, file_name: &str) {
    let mut fs = FS::new(fs_path, secret).unwrap();
    fs.remove_file(path, file_name).unwrap();
}

fn print_file(fs_path: &str, secret: &str, path: &str, file_name: &str) {
    let mut fs = FS::new(fs_path, secret).unwrap();
    let mut d = vec![];
    let mut buf = Cursor::new(&mut d);

    fs.get_file_data(path, file_name, &mut buf).unwrap();

    println!("{}", String::from_utf8_lossy(&d));
}

fn export(fs_path: &str, secret: &str, path: &str, file_name: &str, output: &str) {
    let mut fs = FS::new(fs_path, secret).unwrap();

    let start = Instant::now();

    let mut file = File::create(output).unwrap();
    let finfo = fs.get_file_info(path, file_name).unwrap();

    file.set_len(finfo.size).unwrap();

    fs.get_file_data(path, file_name, &mut file).unwrap();
    file.flush().unwrap();

    let duration = start.elapsed();
    println!("Time alapsed: {} millisec", duration.as_millis());
}

fn init(path: &str, secret: &str) {
    FS::init(path, secret).unwrap();
}
