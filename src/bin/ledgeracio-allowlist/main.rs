// Copyright 2020 Parity Technologies (UK) Ltd.
// This file is part of ledgeracio.
//
// ledgeracio is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, either version 3 of the License, or (at your option) any later
// version.
//
// ledgeracio is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with ledgeracio.  If not, see <http://www.gnu.org/licenses/>.

//! CLI for approved validator list handling

#![deny(clippy::all, clippy::pedantic)]
#![allow(clippy::non_ascii_literal)]
#![forbid(unsafe_code)]

mod keyparse;
mod parser;

/// The version of keys supported
pub const KEY_VERSION: u8 = 1;

/// The magic number at the beginning of a secret key
pub const KEY_MAGIC: &[u8] = &*b"Ledgeracio Secret Key";

use ledgeracio::{get_network, Error, HardStore};
use sp_core::crypto::AccountId32 as AccountId;
use std::{fmt::Debug,
          fs,
          io::{BufReader, BufWriter}};
use structopt::StructOpt;
use substrate_subxt::{sp_core, sp_core::crypto::Ss58AddressFormat};

use ed25519_dalek::Keypair;
use keyparse::{parse_public, parse_secret};
use parser::parse as parse_allowlist;
use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};
use substrate_subxt::sp_core::H256;

async fn inner_main() -> Result<(), Error> {
    env_logger::init();
    let LedgeracioAllowlist { network, cmd } = LedgeracioAllowlist::from_args();

    let keystore = || HardStore::new(network);
    really_inner_main(cmd, keystore, network).await?;
    Ok(())
}

fn main() {
    match async_std::task::block_on(inner_main()) {
        Ok(()) => (),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1)
        }
    }
}

#[derive(StructOpt, Debug)]
#[structopt(
    name = "ledgeracio-allowlist",
    about = "Ledgeracio approved validator management CLI"
)]
struct LedgeracioAllowlist {
    /// Network
    #[structopt(long, parse(try_from_str = get_network))]
    network: Ss58AddressFormat,
    /// Subcommand
    #[structopt(subcommand)]
    cmd: AllowlistCommand,
}

#[derive(StructOpt, Debug)]
pub(crate) enum AllowlistCommand {
    /// Upload a new approved validator list.  This list must be signed.
    Upload { path: PathBuf },
    /// Set the validator list signing key.  This will fail if a signing key has
    /// already been set.
    SetKey {
        /// The file containing the public signing key.  You can generate this
        /// file with `ledgeracio allowlist gen-key`.
        key: PathBuf,
    },
    /// Get the validator list signing key.  This will fail unless a signing key
    /// has been set.
    GetKey,
    /// Generate a new signing key.
    GenKey {
        /// Prefix of the file to write the keys to
        ///
        /// The public key will be written to `file.pub` and the secret key
        /// to `file.sec`.
        file: PathBuf,
    },
    /// Compile the provided textual allowlist into a binary format and sign it.
    ///
    /// `secret` should be a secret signing key generated by `ledgeracio
    /// allowlist genkey`.  If you provide a public key, it will be verified
    /// to match the provided secret key.  This helps check that neither has
    /// been corrupted, and that you are using the correct secret key.
    Sign {
        /// The textual allowlist file.
        ///
        /// The textual allowlist format is very simple.  If a line is empty, or
        /// if its first non-whitespace character is `;` or `#`, it is
        /// considered a comment.  Otherwise, the line must be a valid SS58
        /// address for the provided network, except that leading and
        /// trailing whitespace are ignored.  The process of compiling
        /// an allowlist to binary format and signing it is completely
        /// deterministic.
        #[structopt(short = "f", long = "file")]
        file: PathBuf,
        /// The secret key file.
        #[structopt(short = "s", long = "secret")]
        secret: PathBuf,
        /// The output file
        #[structopt(short = "o", long = "output")]
        output: PathBuf,
        /// The nonce.  This must be greater than any nonce used previously with
        /// the same key, and is used to prevent replay attacks.
        #[structopt(short = "n", long = "nonce")]
        nonce: u32,
    },
    /// Inspect the given allowlist file and verify its signature. The output is
    /// in a format suitable for `ledgeracio sign`.
    Inspect {
        /// The binary allowlist file to read
        #[structopt(short = "f", long = "file")]
        file: PathBuf,
        /// The public key file.
        #[structopt(short = "p", long = "public")]
        public: PathBuf,
        /// The output file.  Defaults to stdout.
        #[structopt(short = "o", long = "output")]
        output: Option<PathBuf>,
    },
}

fn write(buf: &[&[u8]], path: &std::path::Path) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .mode(0o400)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    for i in buf {
        f.write_all(i)?;
    }
    Ok(())
}

async fn really_inner_main<T: FnOnce() -> Result<ledgeracio::HardStore, Error>>(
    acl: AllowlistCommand,
    hardware: T,
    network: Ss58AddressFormat,
) -> Result<Option<H256>, Error> {
    match acl {
        AllowlistCommand::GetKey => {
            let s: [u8; 32] = hardware()?.get_pubkey().await?;
            println!("Public key is {}", base64::encode(s));
        }
        AllowlistCommand::SetKey { key } => {
            let (key, key_network) = parse_public(&*fs::read(key)?)?;
            if key_network != network {
                return Err(format!(
                    "Key is for network {}, not {}",
                    String::from(key_network),
                    String::from(network)
                )
                .into())
            }
            hardware()?.set_pubkey(&key.as_bytes()).await?
        }
        AllowlistCommand::Upload { path } => {
            let allowlist = fs::read(path)?;
            hardware()?.allowlist_upload(&allowlist).await?
        }
        AllowlistCommand::GenKey { mut file } => {
            if file.extension().is_some() {
                return Err(format!(
                    "please provide a filename with no extension, not {}",
                    file.display()
                )
                .into())
            }
            let keypair = Keypair::generate(&mut rand::rngs::OsRng {});
            let secretkey = keypair.secret.to_bytes();
            let publickey = keypair.public.to_bytes();
            file.set_extension("pub");
            let public = format!(
                "Ledgeracio version 1 public key for network {}\n{}\n",
                match network {
                    Ss58AddressFormat::KusamaAccount => "Kusama",
                    Ss58AddressFormat::PolkadotAccount => "Polkadot",
                    _ => unreachable!("should have been rejected earlier"),
                },
                base64::encode(&publickey[..])
            );
            write(&[public.as_bytes()], &file)?;
            file.set_extension("sec");
            write(
                &[
                    KEY_MAGIC,
                    &u16::from(KEY_VERSION).to_le_bytes(),
                    &[network.into()],
                    &secretkey[..],
                    &publickey[..],
                ],
                &file,
            )?;
        }
        AllowlistCommand::Sign {
            file,
            secret,
            output,
            nonce,
        } => {
            let file = BufReader::new(fs::File::open(file)?);
            let secret: Vec<u8> = fs::read(secret)?;
            let Keypair { public, secret } = parse_secret(&*secret, network)?;
            let signed =
                parse_allowlist::<_, AccountId>(file, network, &public, &(&secret).into(), nonce)?;
            fs::write(output, signed)?;
        }
        AllowlistCommand::Inspect {
            file,
            public,
            output,
        } => {
            let file = BufReader::new(fs::File::open(file)?);
            let (pk, network) = parse_public(&*fs::read(public)?)?;
            let stdout = std::io::stdout();
            let mut output = BufWriter::new(match output {
                None => Box::new(stdout.lock()) as Box<dyn std::io::Write>,
                Some(path) => Box::new(
                    OpenOptions::new()
                        .mode(0o600)
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(path)?,
                ),
            });

            for i in crate::parser::inspect::<_, AccountId>(file, network, &pk)? {
                writeln!(output, "{}", i)?;
            }
        }
    }
    Ok(None)
}