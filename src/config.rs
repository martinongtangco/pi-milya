use anyhow::{bail, Result};

#[derive(Clone)]
pub struct Config {
    pub pi_binary: String,
    pub cwd: String,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub no_session: bool,
    pub session_id: Option<String>,
    pub session_dir: Option<String>,
    pub session_name: Option<String>,
    pub tools: Option<String>,
    pub smoke: bool,
    pub system_prompt: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pi_binary: "pi".into(),
            cwd: ".".into(),
            model: None,
            thinking: None,
            no_session: false,
            session_id: None,
            session_dir: None,
            session_name: None,
            tools: None,
            smoke: false,
            system_prompt: None,
        }
    }
}

fn take_value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Option<&'a str> {
    if args[*i] == flag {
        *i += 1;
        args.get(*i).map(|s| s.as_str())
    } else {
        None
    }
}

pub fn parse_args(args: &[String]) -> Result<Config> {
    let mut c = Config::default();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--smoke" => c.smoke = true,
            "--no-session" => c.no_session = true,
            f if f.starts_with('-') => {
                // flag with value
                let v = match take_value(args, &mut i, f) {
                    Some(v) => v,
                    None => bail!("missing value for {f}"),
                };
                match f {
                    "--pi" => c.pi_binary = v.into(),
                    "--cwd" => c.cwd = v.into(),
                    "--model" => c.model = Some(v.into()),
                    "--thinking" => c.thinking = Some(v.into()),
                    "--session-id" => c.session_id = Some(v.into()),
                    "--session-dir" => c.session_dir = Some(v.into()),
                    "--name" => c.session_name = Some(v.into()),
                    "--tools" => c.tools = Some(v.into()),
                    "--system-prompt" => c.system_prompt = Some(v.into()),
                    _ => bail!("unknown flag {f} (see --help)"),
                }
            }
            other => bail!("unexpected argument '{other}' (see --help)"),
        }
        i += 1;
    }
    Ok(c)
}

pub fn print_usage() {
    println!(
        "pi-milya — a Grok-style chat TUI on the pi agent (pi --mode rpc)

Usage: pi-milya [options]

Options:
  --pi <bin>           pi binary (default: pi on PATH)
  --cwd <dir>          working directory for the pi session
  --model <pattern>    pass --model through to pi
  --thinking <level>   pass --thinking through (off|minimal|low|medium|high|xhigh|max)
  --no-session         in-memory session (not persisted)
  --session-id <id>    resume/create a session by id
  --session-dir <dir>  override session storage dir
  --name <name>        session display name
  --tools <list>       pass -t through to pi
  --system-prompt <t>  append a system prompt to pi's default
  --smoke              non-TUI end-to-end check, then exit
  -h, --help           this help

Keys:
  enter        send (steers while the agent is running)
  shift+enter / ctrl+j   newline
  esc          abort (while running) or clear input
  up/down      history
  tab          complete slash commands
  ctrl+n / ctrl+p  next / previous family member
  t            toggle last thinking block
  ?            help
  ctrl+c       quit

First run: a wizard sets up your first family member
(name · model · thinking · ascii face · system prompt).
The family is stored in ~/.config/pi-milya/family.json

Slash: /model /think /new /edit /clear /abort /compact /stats /help /quit
",
    );
}
