# claude-profiles
Run several Claude accounts side by side (personal, work, clients): Claude Code on macOS, Linux and Windows, and Claude Desktop on macOS. Use it from the terminal, the macOS menu bar, or a desktop window that runs on all three.

## Build and install

```bash
# The command
cargo install --path .

# The menu bar app (bundles its own copy of the command)
scripts/build-app.sh --install     # → ~/Applications/Claude Profiles.app
```

Needs Rust and the Xcode command line tools. The app is signed ad hoc, for this Mac only.

## The desktop window (macOS, Linux, Windows)

`gui/` is the same panel as the menu bar app, built with [GPUI](https://gpui.rs) and [gpui-component](https://github.com/longbridge/gpui-component), so one codebase runs everywhere: the Accounts, Running and Files tabs, each account's App and CLI rows, plan usage, sessions, and adding, importing, renaming and removing accounts. Like the menu bar app, it holds no logic: every action is a `claude-profiles app …` call.

```bash
scripts/build-gui.sh             # → dist/claude-profiles-gui and dist/claude-profiles
scripts/build-gui.sh --install   # also copies both to ~/.local/bin (and adds a Linux menu entry)
```

On Windows, run `cargo build --release --workspace` and keep `claude-profiles.exe` beside `claude-profiles-gui.exe`. The window finds the command beside itself first, then on PATH (or set `CLAUDE_PROFILES_BIN`).

Linux needs GPUI's libraries, for example on Debian or Ubuntu:

```bash
sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libx11-xcb-dev \
  libwayland-dev libvulkan-dev libfontconfig1-dev libfreetype6-dev libzstd-dev libasound2-dev
```

What differs off macOS:

- **Claude Code profiles only.** Claude Desktop profiles still need macOS (their launchers are AppleScript apps).
- **Sign-ins** live in each profile folder's `.credentials.json`, as Claude Code keeps them there, so they move with a rename and go with a removal.
- **The command** is a PowerShell function in your PowerShell profile on Windows, and an alias in `.zshrc`, `.bashrc`/`.bash_profile` or fish's `config.fish` on Linux.
- **Terminals**: on Windows, PowerShell or Windows Terminal; on Linux, your `$TERMINAL`, `x-terminal-emulator`, GNOME Terminal, Konsole, Kitty, Alacritty, WezTerm or xterm.
- **The window** is blurred glass on macOS and Windows, and solid on Linux, where transparency without a compositor shows as black.

## The menu bar app

Click the person icon in the menu bar:

- **Every account at a glance.** You see the default install and each profile, with who each one is signed in as (email and organisation for Claude Code, matched to Desktop where possible).
- **Start Claude Code as any account** in one click. Pick a folder to start in, or copy the command. It opens Terminal, iTerm, Ghostty or Warp (chosen in Settings).
- **Live CLI sessions per account.** For each session you get its folder, memory and uptime. You can reveal the folder or end the session.
- **Desktop instances.** Open, show or quit each profile's Claude, with its memory and CPU. It warns when a copy is running without its profile, which means it is on the wrong account.
- **Versions and app copies.** It shows the installed Claude Desktop and Claude Code versions and each profile's copy, whether that copy is current, behind (it updates on its next open) or ahead, plus the copy's and data folder's disk use.
- **Manage profiles.** Add an account, rename, remove (data folders optional), repair a launcher, and switch how a copy updates. Settings also has `doctor --fix`, opens to doctor, extensions and the full add wizard, and an Open at login toggle.

Everything the app does is a call to `claude-profiles app …`, which prints JSON and never prompts. The app has no logic of its own. CI builds and tests the command and the window on macOS, Linux and Windows (`.github/workflows/ci.yml`).
<p align="center">
  <img
    src="https://github.com/user-attachments/assets/5fb69e09-9e12-4d93-a5ff-283ff6e2dfa9"
    width="450"
    alt="Claude Profiles menu bar app"
  />
</p>


## The command

```
claude-profiles add | list | status | doctor [--fix|--undo] | extensions
                rename [old] [new] | repair [name] | remove [name]
                self-update [name] [on|off] | upgrade [--git URL | --path DIR]
```

It works the same as the original, with these differences:

- **Launch helper.** The helper that runs on every Dock click is a copy of this compiled binary (`~/Library/Application Support/claude-profiles/bin/claude-profiles __launch …`), not a JavaScript file run by `osascript`. Every program it runs gets its arguments as a list.
- **Safer deletion.** `remove` and `rename` delete a launcher only if it is an `.app` carrying this tool's bundle ID. A hand-edited registry cannot point them at something else.
- **GitHub CLI isolation.** New profiles with their own GitHub CLI login get `GH_CONFIG_DIR` in their Desktop launcher straight away. The original needed a `doctor --fix` for that.
- **Upgrades.** `upgrade` reinstalls with `cargo install --locked` from a git URL or a folder you name, instead of from npm. Read the source first if it is not your own.
- **Separate from the original.** It uses its own folders (`~/.config/claude-profiles`, `~/Library/Application Support/claude-profiles`, bundle IDs `com.claude-profiles.*`), so it does not touch profiles made by the original.

The same limits apply as in the original. Profiles are separated by configuration, not sandboxed: anything running as you can read every profile's files. Sign new Desktop profiles in with the emailed code, not Google, while another Claude is open.

## Development

```bash
cargo test --workspace   # some tests compile real launchers (macOS only)
```

Note: an unrelated tool also called `claude-profiles` keeps its data in `~/.claude-profiles`. This tool never uses that folder, and refuses the profile name `profiles`, which would collide with it.
