<img width="999" height="1575" alt="image" src="https://github.com/user-attachments/assets/5fb69e09-9e12-4d93-a5ff-283ff6e2dfa9" /># claude-profiles

Run several Claude accounts side by side on macOS (personal, work, clients), in Claude Desktop and Claude Code, from the terminal or the menu bar.

## Build and install

```bash
# The command
cargo install --path .

# The menu bar app (bundles its own copy of the command)
scripts/build-app.sh --install     # → ~/Applications/Claude Profiles.app
```

Needs Rust and the Xcode command line tools. The app is signed ad hoc, for this Mac only.

## The menu bar app

Click the person icon in the menu bar:

- **Every account at a glance.** You see the default install and each profile, with who each one is signed in as (email and organisation for Claude Code, matched to Desktop where possible).
- **Start Claude Code as any account** in one click. Pick a folder to start in, or copy the command. It opens Terminal, iTerm, Ghostty or Warp (chosen in Settings).
- **Live CLI sessions per account.** For each session you get its folder, memory and uptime. You can reveal the folder or end the session.
- **Desktop instances.** Open, show or quit each profile's Claude, with its memory and CPU. It warns when a copy is running without its profile, which means it is on the wrong account.
- **Versions and app copies.** It shows the installed Claude Desktop and Claude Code versions and each profile's copy, whether that copy is current, behind (it updates on its next open) or ahead, plus the copy's and data folder's disk use.
- **Manage profiles.** Add an account, rename, remove (data folders optional), repair a launcher, and switch how a copy updates. Settings also has `doctor --fix`, opens to doctor, extensions and the full add wizard, and an Open at login toggle.

Everything the app does is a call to `claude-profiles app …`, which prints JSON and never prompts. The app has no logic of its own.
![Uploading image.png…](Menu bar)


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
cargo test            # 72 tests; some compile real launchers (macOS only)
```

Note: an unrelated tool also called `claude-profiles` keeps its data in `~/.claude-profiles`. This tool never uses that folder, and refuses the profile name `profiles`, which would collide with it.
