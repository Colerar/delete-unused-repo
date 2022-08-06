use std::collections::HashMap;
use std::process::exit;
use std::sync::Arc;
use std::time::{Instant};

use clap::builder::PossibleValuesParser;
use clap::Parser;
use console::{Emoji, style};
use indicatif::{HumanDuration, ProgressBar, ProgressStyle};
use log::{debug, error, info};
use octocrab::models::Repository;

#[derive(Parser, Debug, Clone)]
#[clap(name = "delete-unused-repo", version, about, long_about = None)]
struct Cli {
  /// GitHub Token
  #[clap(short, long, value_parser)]
  token: String,
  /// Only delete forks
  #[clap(short, long, value_parser, default_value_t = true)]
  fork: bool,
  /// Delete certain visibility value
  #[clap(short, long, value_parser = PossibleValuesParser::from(vec!["public", "internal", "private"]), default_value = "public")]
  visibility: Vec<String>,
  /// Owner, maybe yourself or organization you have access
  #[clap(short, long)]
  owner: Option<Vec<String>>,
  /// Delete if stars number <= [STARS]
  #[clap(short, long, value_parser, default_value_t = 0, value_name = "STARS")]
  star: u32,
}

#[derive(Debug, Clone)]
struct WrappedRepo(Repository);

static LOOKING_GLASS: Emoji<'_, '_> = Emoji("🔍  ", "");
static CLIP: Emoji<'_, '_> = Emoji("🔗  ", "");
static FILTER: Emoji<'_, '_> = Emoji("⏳  ", "");
static TRASH: Emoji<'_, '_> = Emoji("🗑  ", "");
static SPARKLE: Emoji<'_, '_> = Emoji("✨ ", ":-)");

#[tokio::main]
async fn main() {
  let started = Instant::now();
  if std::env::var("RUST_LOG").is_err() {
    std::env::set_var("RUST_LOG", "info");
  }
  pretty_env_logger::init();
  let args: Cli = Cli::parse();
  debug!("{:?}", args);

  let spinner_style = ProgressStyle::with_template("{prefix:.bold.dim} {spinner} {wide_msg}")
    .unwrap()
    .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ");

  info!(
    "{} {}Login to GitHub...",
    style("[1/4]").bold().dim(),
    CLIP
  );

  let gh = octocrab::Octocrab::builder()
    .personal_token(args.token)
    .build();
  let gh = match gh {
    Ok(gh) => gh,
    Err(e) => {
      error!("Failed to login GitHub via personal token: {e}");
      exit(1);
    }
  };
  let gh = Arc::new(gh);

  info!(
    "{} {}Search repos...",
    style("[2/4]").bold().dim(),
    LOOKING_GLASS
  );


  let get_repos = |page: u8| {
    let gh = Arc::clone(&gh);
    async move {
      let page = match gh
        .current()
        .list_repos_for_authenticated_user()
        .per_page(100)
        .page(page)
        .send()
        .await
      {
        Ok(page) => page,
        Err(e) => {
          error!("Failed to get GitHub repos of you: {e}");
          exit(1);
        }
      };
      page
    }
  };

  let mut repos = vec![];
  {
    let first = get_repos(1).await;
    let page_num = first.number_of_pages();
    repos.extend(first);
    if page_num >= Some(2) {
      let (tx, mut rx) = tokio::sync::mpsc::channel(32);
      for i in 2..=page_num.unwrap() {
        let i = i as u8;
        let handle = tokio::spawn(get_repos(i));
        tx.send(handle).await.unwrap();
      }
      drop(tx);

      while let Some(get_repo) = rx.recv().await {
        repos.extend(get_repo.await.unwrap().items);
      }
    }
  };

  info!(
    "{} {}Filter repos...",
    style("[3/4]").bold().dim(),
    FILTER,
  );

  let repos: Vec<_> = repos
    .into_iter()
    .filter(|r| {
      if let Some(user) = r.owner.clone().map(|u| u.login) {
        if let Some(owner) = &args.owner {
          owner.contains(&user)
        } else {
          true
        }
      } else {
        true
      }
    })
    .filter(|r| {
      if let Some(vis) = &r.visibility {
        args.visibility.contains(vis)
      } else {
        true
      }
    })
    .filter(|r| r.fork == Some(args.fork))
    .filter(|r| r.stargazers_count <= Some(args.star))
    .collect();

  if repos.is_empty() {
    info!("No matched repos");
    exit(0);
  }

  let iter: Vec<_> = repos
    .into_iter()
    .map(|r| (r.full_name.clone().unwrap(), r))
    .collect();
  let map: HashMap<_, _> = HashMap::from_iter(iter);

  let keys = map.keys().collect::<Vec<_>>();
  let result = dialoguer::MultiSelect::new()
    .with_prompt(
      "These repos will be deleted, \n\
      [Space] to check item, \n\
      [Esc/q] to cancel, \n\
      [Enter] to confirm",
    )
    .items(&keys)
    .defaults(&*vec![true; keys.len()])
    .interact_opt();

  if result.is_err() || (result.is_ok() && result.as_ref().unwrap().is_none()) {
    info!("Cancelled");
    exit(1);
  }

  let confirm_str = "I want to remove all repos above".to_string();
  let confirm: std::io::Result<String> = dialoguer::Input::new()
    .with_prompt(format!("Double confirm, please type '{confirm_str}'"))
    .interact();
  if confirm.is_ok() && confirm.unwrap() == confirm_str {
  } else {
    info!("Cancelled");
    exit(1);
  };


  let repos: Vec<_> = if let Ok(Some(to_del)) = result {
    to_del.into_iter().map(|idx| map[keys[idx]].clone()).collect()
  } else {
    info!("Cancelled");
    exit(0);
  };

  let p1 = Arc::new(ProgressBar::new(repos.len() as u64));
  p1.set_style(spinner_style);
  p1.set_prefix("");
  drop(map);
  let (tx, mut rx) = tokio::sync::mpsc::channel(64);
  for repo in repos {
    let owner = repo.owner.map(|a| a.login);
    if owner.is_none() { return; }
    let owner = owner.unwrap();
    let repo = repo.name;
    let gh = Arc::clone(&gh);
    let p1 = Arc::clone(&p1);
    let handle = async move {
      if let Err(err) = gh.repos(&owner, &repo).delete().await {
        error!("Failed to delete {}/{}: {:?}", owner, repo, err);
      }
      p1.set_message(format!("Deleted {}/{}", owner, repo));
      p1.inc(1);
    };
    tx.send(tokio::spawn(handle)).await.unwrap();
  }
  drop(tx);
  while let Some(handle) = rx.recv().await {
    handle.await.unwrap();
  }
  info!("{} {} Delete repos", style("[4/4]").bold().dim(), TRASH);
  info!("{} Done in {}", SPARKLE, HumanDuration(started.elapsed()));
}
