// Claude Profiles: the menu bar face of the `claude-profiles` command.
//
// The app holds no logic of its own. Everything it shows comes from
// `claude-profiles app state`, and everything it does is a
// `claude-profiles app <action>` call. The command ships inside this bundle
// (Contents/Resources/claude-profiles), so the app works without anything on
// PATH; it falls back to a copy on PATH when run unbundled.

import AppKit
import ServiceManagement
import SwiftUI

// MARK: - Model (the JSON `app state` prints)

struct Account: Codable, Hashable {
    var email: String?
    var name: String?
    var organization: String?
    var accountUuid: String?
    var plan: String?
}

struct DesktopAccount: Codable, Hashable {
    var accountUuid: String
    var matched: Account?
    /// A name you gave it, for an account only an ID identifies.
    var nickname: String?
}

struct Session: Codable, Hashable, Identifiable {
    var pid: Int
    var cwd: String?
    var uptime: String
    var memoryMB: Int
    var cpu: Double
    var fromDesktop: Bool
    var id: Int { pid }
}

struct Clone: Codable, Hashable {
    var path: String
    var exists: Bool
    var version: String?
    var state: String
    var colorIntact: Bool
    var running: Bool
    var wrongAccountPids: [Int]
    var memoryMB: Int
    var cpu: Double
}

struct DesktopHalf: Codable, Hashable {
    var dataDir: String
    var appPath: String
    var launcherExists: Bool
    var color: String?
    var updates: String
    var account: DesktopAccount?
    var clone: Clone
}

struct CodeHalf: Codable, Hashable {
    var configDir: String
    var alias: String
    var ghConfigDir: String?
    var account: Account?
    var sessions: [Session]
    /// Started by Claude Desktop for its Code features, not by you.
    var desktopSessions: [Session]?
}

struct ProfileState: Codable, Hashable, Identifiable {
    var name: String
    var kind: String
    var createdAt: String
    var desktop: DesktopHalf?
    var code: CodeHalf?
    var id: String { name }
}

struct AppInfo: Codable, Hashable { var path: String; var version: String? }
struct CliInfo: Codable, Hashable { var path: String; var version: String?; var shadowed: [String] }
struct ClaudeInfo: Codable, Hashable { var desktopApp: AppInfo?; var cli: CliInfo? }

struct DefaultDesktop: Codable, Hashable {
    var dataDir: String
    var account: DesktopAccount?
    var running: Bool
    var memoryMB: Int
    var cpu: Double
}

struct DefaultCode: Codable, Hashable {
    var configDir: String
    var account: Account?
    var sessions: [Session]
    var desktopSessions: [Session]?
}

struct Defaults: Codable, Hashable { var desktop: DefaultDesktop?; var code: DefaultCode }
struct OtherSessions: Codable, Hashable { var configDir: String; var sessions: [Session] }

struct State: Codable {
    var version: String
    var isMac: Bool
    var helper: String
    var ghAvailable: Bool
    var colors: [String]
    var claude: ClaudeInfo
    var `default`: Defaults
    var otherSessions: [OtherSessions]
    var profiles: [ProfileState]
}

struct SizeRow: Codable { var name: String; var desktopDataMB: Int?; var cloneMB: Int?; var codeConfigMB: Int? }
struct Sizes: Codable { var `default`: SizeRow?; var profiles: [SizeRow] }

struct ImportCandidate: Codable, Hashable, Identifiable {
    var name: String
    var desktopDir: String?
    var desktopAccount: String?
    var codeDir: String?
    var codeAccount: Account?
    var running: Bool
    var id: String { (desktopDir ?? "") + "|" + (codeDir ?? "") }
}
struct Importable: Codable { var candidates: [ImportCandidate] }

struct UsageLimit: Codable { var percent: Double?; var resetsAt: String? }
/// One limit as the command itemises it: "session", "weekly_all", or one
/// scoped to a model (such as a weekly allowance for one model).
struct LimitItem: Codable { var kind: String?; var percent: Double?; var resetsAt: String?; var model: String? }
struct Usage: Codable {
    var status: String
    var limits: [LimitItem]?
    var fiveHour: UsageLimit?
    var sevenDay: UsageLimit?
    var sevenDayOpus: UsageLimit?
    var sevenDaySonnet: UsageLimit?
}
struct UsageRow: Codable { var name: String; var usage: Usage }
struct Usages: Codable { var `default`: Usage?; var profiles: [UsageRow] }

// MARK: - Talking to the command

enum CLI {
    /// The bundled command, or one on PATH when the app runs unbundled.
    static let binary: String? = {
        if let bundled = Bundle.main.url(forResource: "claude-profiles", withExtension: nil)?.path,
           FileManager.default.isExecutableFile(atPath: bundled) {
            return bundled
        }
        let home = NSHomeDirectory()
        for dir in ["\(home)/.cargo/bin", "/opt/homebrew/bin", "/usr/local/bin", "\(home)/.local/bin"] {
            let p = "\(dir)/claude-profiles"
            if FileManager.default.isExecutableFile(atPath: p) { return p }
        }
        return nil
    }()

    /// Apps started from the Dock or at login get a bare PATH, so `claude`,
    /// `gh` and `npm` would be invisible to the command. Use the login
    /// shell's PATH, plus the usual install folders.
    static let environment: [String: String] = {
        var env = ProcessInfo.processInfo.environment
        let home = NSHomeDirectory()
        var parts: [String] = []
        let shell = env["SHELL"] ?? "/bin/zsh"
        let p = Process()
        p.executableURL = URL(fileURLWithPath: shell)
        p.arguments = ["-l", "-c", "printf %s \"$PATH\""]
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        if (try? p.run()) != nil {
            p.waitUntilExit()
            if let s = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) {
                parts += s.split(separator: ":").map(String.init)
            }
        }
        parts += ["/opt/homebrew/bin", "/usr/local/bin", "\(home)/.local/bin", "\(home)/.cargo/bin",
                  "\(home)/.npm-global/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        var seen = Set<String>()
        env["PATH"] = parts.filter { !$0.isEmpty && seen.insert($0).inserted }.joined(separator: ":")
        return env
    }()

    struct Failure: Error, LocalizedError {
        var message: String
        var errorDescription: String? { message }
    }

    /// Run `claude-profiles app <args>` and return its stdout.
    static func run(_ args: [String]) async throws -> Data {
        guard let bin = binary else {
            throw Failure(message: "claude-profiles was not found. Rebuild the app with scripts/build-app.sh.")
        }
        return try await withCheckedThrowingContinuation { cont in
            DispatchQueue.global(qos: .userInitiated).async {
                let p = Process()
                p.executableURL = URL(fileURLWithPath: bin)
                p.arguments = ["app"] + args
                p.environment = environment
                let out = Pipe(), err = Pipe()
                p.standardOutput = out
                p.standardError = err
                do { try p.run() } catch {
                    cont.resume(throwing: Failure(message: error.localizedDescription))
                    return
                }
                let data = out.fileHandleForReading.readDataToEndOfFile()
                let errData = err.fileHandleForReading.readDataToEndOfFile()
                p.waitUntilExit()
                if p.terminationStatus == 0 {
                    cont.resume(returning: data)
                } else {
                    // The reason is the last ✗ line, or failing that the last line.
                    let text = String(data: errData, encoding: .utf8) ?? ""
                    let lines = text.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }
                    let reason = lines.last(where: { $0.hasPrefix("✗") }).map { String($0.dropFirst(2)) }
                        ?? lines.last ?? "claude-profiles exited with status \(p.terminationStatus)."
                    cont.resume(throwing: Failure(message: reason))
                }
            }
        }
    }
}

// MARK: - Store

@MainActor
final class Store: ObservableObject {
    @Published var state: State?
    @Published var sizes: [String: SizeRow] = [:]
    @Published var candidates: [ImportCandidate] = []
    /// A profile card to scroll to, and the one briefly highlighted.
    @Published var scrollTarget: String?
    @Published var highlighted: String?

    func reveal(_ name: String) {
        scrollTarget = name
        highlighted = name
        Task {
            try? await Task.sleep(nanoseconds: 1_600_000_000)
            if self.highlighted == name { self.highlighted = nil }
        }
    }
    /// Plan usage by profile name, "default" for the default account.
    @Published var usage: [String: Usage] = [:]
    @Published var error: String?
    @Published var busy: String?
    @Published var notice: String?
    @AppStorage("terminal") var terminal = Store.installedTerminal

    /// Until one is picked in Settings: the terminal app installed alongside
    /// Apple's, if any, since that is the one people use.
    static var installedTerminal: String {
        let known = [("Warp", "dev.warp.Warp-Stable"), ("iTerm", "com.googlecode.iterm2"), ("Ghostty", "com.mitchellh.ghostty")]
        return known.first { NSWorkspace.shared.urlForApplication(withBundleIdentifier: $0.1) != nil }?.0 ?? "Terminal"
    }
    private var helperTried = false

    private var timer: Timer?
    private var ticks = 0
    private var signedIn: Set<String> = []

    func start() {
        refresh()
        loadUsage()
        loadImportable()
        timer?.invalidate()
        ticks = 0
        timer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                self.refresh()
                // Usage asks Anthropic's servers, so once a minute, not every tick.
                self.ticks += 1
                if self.ticks % 12 == 0 { self.loadUsage() }
            }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
    }

    func refresh() {
        Task {
            do {
                let data = try await CLI.run(["state"])
                let state = try JSONDecoder().decode(State.self, from: data)
                self.state = state
                // Someone signed in or out: fetch their usage now, not in a minute.
                var now: Set<String> = state.default.code.account != nil ? ["default"] : []
                for p in state.profiles where p.code?.account != nil { now.insert(p.name) }
                if now != self.signedIn { self.signedIn = now; self.loadUsage() }
                // A reinstall leaves the launchers' helper a build behind. Bring
                // it level once, quietly; the badge stays if that fails.
                if state.helper != "current" && !self.helperTried && state.profiles.contains(where: { $0.desktop != nil }) {
                    self.helperTried = true
                    _ = try? await CLI.run(["update-helper"])
                    self.refresh()
                }
                if self.error?.hasPrefix("Could not read") == true { self.error = nil }
            } catch {
                self.error = "Could not read state: \(error.localizedDescription)"
            }
        }
    }

    func loadSizes() {
        Task {
            if let data = try? await CLI.run(["sizes"]),
               let s = try? JSONDecoder().decode(Sizes.self, from: data) {
                var map = Dictionary(uniqueKeysWithValues: s.profiles.map { ($0.name, $0) })
                if let d = s.default { map["default"] = d }
                self.sizes = map
            }
        }
    }

    func loadImportable() {
        Task {
            if let data = try? await CLI.run(["importable"]),
               let i = try? JSONDecoder().decode(Importable.self, from: data) {
                self.candidates = i.candidates
            }
        }
    }

    /// The card that shows a Claude Code account's usage: the first profile
    /// whose Code half uses it, else Default. Profiles come first, so an
    /// account shared with Default shows in the profile's card and Default
    /// points there. Desktop halves are left out: only Code shows usage.
    func firstOwner(of uuid: String) -> String? {
        guard let s = state else { return nil }
        if let p = s.profiles.first(where: { $0.code?.account?.accountUuid == uuid }) { return p.name }
        return s.default.code.account?.accountUuid == uuid ? "default" : nil
    }

    func loadUsage() {
        Task {
            if let data = try? await CLI.run(["usage"]),
               let u = try? JSONDecoder().decode(Usages.self, from: data) {
                var map = Dictionary(uniqueKeysWithValues: u.profiles.map { ($0.name, $0.usage) })
                if let d = u.default { map["default"] = d }
                self.usage = map
            }
        }
    }

    /// Run an action, showing `label` while it runs, then refresh.
    func act(_ label: String, _ args: [String], done: String? = nil, then: (() -> Void)? = nil) {
        busy = label
        error = nil
        Task {
            defer { self.busy = nil; self.refresh() }
            do {
                _ = try await CLI.run(args)
                if let done { self.flash(done) }
                then?()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    func flash(_ text: String) {
        notice = text
        Task {
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            if self.notice == text { self.notice = nil }
        }
    }

    func copyCommand(_ target: String) {
        Task {
            if let data = try? await CLI.run(["cli-command", target]),
               let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let cmd = obj["command"] as? String {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(cmd, forType: .string)
                self.flash("Command copied")
            }
        }
    }

    func openCLI(_ target: String, inFolder: Bool) {
        var args = ["open-cli", target, "--terminal", terminal]
        if inFolder {
            let panel = NSOpenPanel()
            panel.canChooseDirectories = true
            panel.canChooseFiles = false
            panel.prompt = "Start Claude Here"
            panel.message = "Choose the folder to start Claude Code in"
            NSApp.activate(ignoringOtherApps: true)
            guard panel.runModal() == .OK, let url = panel.url else { return }
            args += ["--dir", url.path]
        }
        act("Opening \(terminal)…", args)
    }

    /// Open a terminal running Claude Code's sign-in for this account.
    func signIn(_ target: String) {
        act("Opening \(terminal)…", ["open-cli", target, "--login", "--terminal", terminal])
    }
}

// MARK: - Small views

extension Color {
    static func named(_ c: String?) -> Color {
        switch c {
        case "orange": return .orange
        case "red": return .red
        case "yellow": return .yellow
        case "green": return .green
        case "teal": return .teal
        case "blue": return .blue
        case "purple": return .purple
        case "pink": return .pink
        default: return .secondary
        }
    }
}

/// The plan limits Claude Code's /usage shows, as small bars.
struct UsageBars: View {
    var usage: Usage?
    /// Offered when the folder names an account but its Keychain sign-in is
    /// gone, as after an older rename.
    var signIn: (() -> Void)? = nil
    var body: some View {
        if let u = usage {
            switch u.status {
            case "ok":
                VStack(alignment: .leading, spacing: 3) {
                    if let items = u.limits, !items.isEmpty {
                        ForEach(Array(items.enumerated()), id: \.offset) { _, l in
                            bar(label(l), UsageLimit(percent: l.percent, resetsAt: l.resetsAt))
                        }
                    } else {
                        if let l = u.fiveHour { bar("5 hours", l) }
                        if let l = u.sevenDay { bar("Week", l) }
                        if let l = u.sevenDayOpus { bar("Opus week", l) }
                        if let l = u.sevenDaySonnet { bar("Sonnet week", l) }
                    }
                    // Limit resets and extra usage are offered on claude.ai,
                    // for the account signed in there.
                    Button {
                        NSWorkspace.shared.open(URL(string: "https://claude.ai/settings/usage")!)
                    } label: {
                        Label("Limit resets and extra usage on claude.ai", systemImage: "arrow.up.right.square")
                    }
                    .buttonStyle(.link)
                    .font(.system(size: 10))
                }
            case "expired":
                note("Usage shows after its next Claude Code session refreshes the sign-in.")
            case "unavailable":
                note("Usage unavailable right now.")
            case "signed-out":
                HStack(spacing: 6) {
                    note("Its saved sign-in is missing, so usage can't be read. Sign in again to see it.")
                    Spacer(minLength: 4)
                    if let signIn { Button("Sign in", action: signIn).controlSize(.small) }
                }
            default:
                EmptyView()
            }
        }
    }

    func label(_ l: LimitItem) -> String {
        switch (l.kind, l.model) {
        case ("session", _): return "5 hours"
        case ("weekly_all", _): return "Week"
        case (_, let model?): return "\(model) week"
        default: return (l.kind ?? "Limit").replacingOccurrences(of: "_", with: " ")
        }
    }

    func bar(_ label: String, _ l: UsageLimit) -> some View {
        let pct = max(0, min(100, l.percent ?? 0))
        let tint: Color = pct >= 90 ? .red : pct >= 70 ? .orange : .accentColor
        return HStack(spacing: 6) {
            Text(label).foregroundStyle(.secondary).frame(width: 72, alignment: .leading)
            ProgressView(value: pct, total: 100).tint(tint)
            Text("\(Int(pct.rounded()))%").monospacedDigit().frame(width: 34, alignment: .trailing)
            Text(resets(l.resetsAt)).foregroundStyle(.secondary).lineLimit(1).fixedSize()
        }
        .font(.system(size: 10))
    }

    func note(_ text: String) -> some View {
        Text(text).font(.system(size: 10)).foregroundStyle(.secondary)
    }

    func resets(_ iso: String?) -> String {
        guard let iso else { return "" }
        let parser = ISO8601DateFormatter()
        parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = parser.date(from: iso) ?? ISO8601DateFormatter().date(from: iso) else { return "" }
        let f = DateFormatter()
        f.dateFormat = Calendar.current.isDateInToday(date) ? "h:mm a" : "EEE h:mm a"
        return "resets " + f.string(from: date)
    }
}

/// What a typed profile name becomes: lowercase, spaces and underscores as
/// hyphens, anything else dropped.
func profileName(_ typed: String) -> String {
    var out = ""
    for ch in typed.lowercased() {
        if ch == " " || ch == "_" || ch == "-" {
            if !out.hasSuffix("-") { out.append("-") }
        } else if ch.isASCII && (ch.isLetter || ch.isNumber) {
            out.append(ch)
        }
    }
    return out
}

/// Which Claude a row is about, in words as well as an icon: the two icons
/// alone look alike at this size.
struct KindTag: View {
    enum Kind { case app, cli, both }
    var kind: Kind
    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: kind == .cli ? "terminal" : "macwindow")
            Text(kind == .app ? "App" : kind == .cli ? "CLI" : "App + CLI").font(.system(size: 9, weight: .semibold))
        }
        .foregroundStyle(.secondary)
        .padding(.horizontal, 5).padding(.vertical, 2)
        .background(RoundedRectangle(cornerRadius: 4).fill(Color.secondary.opacity(0.12)))
        .help(kind == .app ? "Claude Desktop" : kind == .cli ? "Claude Code" : "Claude Desktop and Claude Code, signed in to the same account")
        .fixedSize()
    }
}

/// Lets you name a Desktop account that no Claude Code sign-in identifies.
/// Claude Desktop keeps only the account's ID in readable form.
struct NameAccountButton: View {
    @EnvironmentObject var store: Store
    var account: DesktopAccount?
    var body: some View {
        if let a = account, a.matched == nil {
            IconButton(symbol: "pencil", help: "Name this account (Claude Desktop only records its ID)") {
                nameAccount(a, store: store)
            }
        }
    }
}

/// Ask for a name for a Desktop account only an ID identifies, and save it.
@MainActor
func nameAccount(_ a: DesktopAccount, store: Store) {
    let alert = NSAlert()
    alert.messageText = "Name this Claude Desktop account"
    alert.informativeText = "Claude Desktop keeps only the account's ID (\(a.accountUuid.prefix(8))…) where this app can read it. Give it the name you know it by, such as the one at the bottom of Claude's sidebar. Leave empty to clear."
    let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
    field.stringValue = a.nickname ?? ""
    field.placeholderString = "e.g. Collins"
    alert.accessoryView = field
    alert.addButton(withTitle: "Save")
    alert.addButton(withTitle: "Cancel")
    NSApp.activate(ignoringOtherApps: true)
    alert.window.initialFirstResponder = field
    if alert.runModal() == .alertFirstButtonReturn {
        store.act("Saving…", ["name-account", a.accountUuid, field.stringValue])
    }
}

struct Badge: View {
    var text: String
    var tint: Color = .secondary
    var body: some View {
        Text(text)
            .font(.system(size: 10, weight: .medium))
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(tint.opacity(0.15), in: Capsule())
            .foregroundStyle(tint)
            .lineLimit(1)
            .fixedSize()
    }
}

/// The ⋯ menu at the end of an App or CLI row: that part's own actions.
struct RowMenu<Content: View>: View {
    @ViewBuilder var content: () -> Content
    var body: some View {
        Menu(content: content) { Image(systemName: "ellipsis") }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("More for this part")
    }
}

func reveal(_ path: String) {
    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
}

struct IconButton: View {
    var symbol: String
    var help: String
    var action: () -> Void
    var body: some View {
        Button(action: action) { Image(systemName: symbol).frame(width: 18, height: 18) }
            .buttonStyle(.borderless)
            .help(help)
    }
}

func accountLabel(_ a: Account?) -> String? {
    guard let a else { return nil }
    let who = a.email ?? a.name
    if let org = a.organization, let who, !org.isEmpty, !org.lowercased().contains(who.lowercased()) {
        return "\(who) · \(org)"
    }
    return who
}

func desktopAccountLabel(_ a: DesktopAccount?) -> String {
    guard let a else { return "Not signed in" }
    if let m = accountLabel(a.matched) { return m }
    if let n = a.nickname, !n.isEmpty { return n }
    return "Account \(a.accountUuid.prefix(8))"
}

func shortPath(_ p: String?) -> String {
    guard let p else { return "?" }
    let home = NSHomeDirectory()
    return p.hasPrefix(home) ? "~" + p.dropFirst(home.count) : p
}

func memory(_ mb: Int) -> String {
    mb >= 1024 ? String(format: "%.1f GB", Double(mb) / 1024) : "\(mb) MB"
}

struct SessionsList: View {
    @EnvironmentObject var store: Store
    var sessions: [Session]
    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(sessions) { s in
                HStack(spacing: 6) {
                    Image(systemName: s.fromDesktop ? "macwindow" : "terminal")
                        .foregroundStyle(.secondary).font(.system(size: 10))
                    Text(shortPath(s.cwd)).font(.system(size: 11)).lineLimit(1).truncationMode(.head)
                    Spacer(minLength: 4)
                    Text("\(memory(s.memoryMB)) · \(s.uptime)").font(.system(size: 10)).foregroundStyle(.secondary)
                    IconButton(symbol: "folder", help: "Show this session's folder in Finder") {
                        if let cwd = s.cwd { NSWorkspace.shared.open(URL(fileURLWithPath: cwd)) }
                    }
                    IconButton(symbol: "stop.circle", help: "End this session (pid \(s.pid))") {
                        confirm("End this Claude Code session?",
                                "Process \(s.pid) in \(shortPath(s.cwd)). Anything it is in the middle of stops.",
                                button: "End Session") {
                            store.act("Ending session…", ["stop-session", String(s.pid)], done: "Session ended")
                        }
                    }
                }
            }
        }
    }
}

/// A standard confirmation alert. Used for everything destructive.
@MainActor
func confirm(_ title: String, _ text: String, button: String, destructive: Bool = true, _ action: () -> Void) {
    let alert = NSAlert()
    alert.messageText = title
    alert.informativeText = text
    alert.alertStyle = destructive ? .warning : .informational
    alert.addButton(withTitle: button)
    alert.addButton(withTitle: "Cancel")
    if destructive { alert.buttons.first?.hasDestructiveAction = true }
    NSApp.activate(ignoringOtherApps: true)
    if alert.runModal() == .alertFirstButtonReturn { action() }
}

// MARK: - Account cards

struct CLIButtons: View {
    @EnvironmentObject var store: Store
    var target: String
    var body: some View {
        HStack(spacing: 6) {
            Button { store.openCLI(target, inFolder: false) } label: {
                Label("Start Claude Code", systemImage: "terminal")
            }
            .controlSize(.small)
            Button { store.openCLI(target, inFolder: true) } label: { Label("In folder…", systemImage: "folder") }
                .controlSize(.small)
            IconButton(symbol: "doc.on.doc", help: "Copy the command that starts this account") { store.copyCommand(target) }
        }
    }
}

struct DefaultCard: View {
    @EnvironmentObject var store: Store
    var state: State
    var body: some View {
        let code = state.default.code
        let desktop = state.default.desktop
        // One row when Desktop and Code are signed in to the same account.
        let merged = desktop?.account != nil && desktop?.account?.accountUuid == code.account?.accountUuid
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Image(systemName: "person.crop.circle").foregroundStyle(.secondary)
                    Text("Default").font(.headline)
                    // Your own install, left as it is: this app shows it but
                    // never adds it as a profile or changes it.
                    Badge(text: "not managed")
                        .help("Your existing Claude install. This app shows it but never changes it.")
                    Spacer()
                }
                if let d = desktop?.account, let c = code.account, d.accountUuid != c.accountUuid {
                    Text("Claude Desktop and Claude Code are signed in to different accounts here.")
                        .font(.system(size: 10)).foregroundStyle(.secondary)
                }
                if let d = desktop {
                    HStack(spacing: 6) {
                        KindTag(kind: merged ? .both : .app)
                        Text(merged ? (accountLabel(code.account) ?? "") : desktopAccountLabel(d.account))
                            .font(.system(size: 12)).lineLimit(1)
                        if !merged { NameAccountButton(account: d.account) }
                        Spacer()
                        if d.running {
                            Text("\(memory(d.memoryMB)) · \(String(format: "%.0f", d.cpu))%").font(.system(size: 10)).foregroundStyle(.secondary)
                        }
                        if merged && !code.sessions.isEmpty {
                            Badge(text: "\(code.sessions.count) running", tint: .green)
                        }
                        Button(d.account == nil && !d.running ? "Sign in" : "Open") {
                            store.act("Opening Claude…", ["open-default-desktop"])
                        }.controlSize(.small)
                        RowMenu {
                            Button("Open") { store.act("Opening Claude…", ["open-default-desktop"]) }
                            if let a = d.account, a.matched == nil {
                                Button(a.nickname == nil ? "Name this account…" : "Rename this account…") { nameAccount(a, store: store) }
                            }
                            Divider()
                            Button("Show data folder") { reveal(d.dataDir) }
                        }
                    }
                }
                if !merged {
                    HStack(spacing: 6) {
                        KindTag(kind: .cli)
                        Text(accountLabel(code.account) ?? "Claude Code: not signed in").font(.system(size: 12)).lineLimit(1)
                        Spacer()
                        if !code.sessions.isEmpty {
                            Badge(text: "\(code.sessions.count) running", tint: .green)
                        }
                        if code.account == nil {
                            Button("Sign in") { store.signIn("default") }.controlSize(.small)
                        }
                        RowMenu {
                            Button("Start Claude Code") { store.openCLI("default", inFolder: false) }
                            Button("Start in folder…") { store.openCLI("default", inFolder: true) }
                            Button("Copy command") { store.copyCommand("default") }
                            if code.account == nil { Button("Sign in…") { store.signIn("default") } }
                            Divider()
                            Button("Show config folder") { reveal(code.configDir) }
                        }
                    }
                }
                if let uuid = code.account?.accountUuid, let owner = store.firstOwner(of: uuid), owner != "default" {
                    HStack(spacing: 6) {
                        Label("Usage: same account as \(owner)", systemImage: "link")
                            .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                        Spacer(minLength: 4)
                        Button { store.reveal(owner) } label: {
                            Label("View", systemImage: "arrow.down")
                        }
                        .controlSize(.small)
                        .help("Scroll to \(owner)'s usage")
                    }
                } else if code.account != nil {
                    UsageBars(usage: store.usage["default"]) { store.signIn("default") }
                }
                CLIButtons(target: "default")
            }
            .padding(4)
        }
    }
}

struct ProfileCard: View {
    @EnvironmentObject var store: Store
    var profile: ProfileState
    var size: SizeRow?
    var onRename: () -> Void
    var onRemove: () -> Void
    var onRemovePart: (String) -> Void = { _ in }
    @State private var expanded = false

    /// Desktop and Code signed in to one account: shown as one row.
    var merged: Bool {
        guard let d = profile.desktop?.account, let c = profile.code?.account else { return false }
        return d.accountUuid == c.accountUuid
    }

    var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                header
                if let d = profile.desktop { desktop(d) }
                if let c = profile.code { code(c) }
                if expanded { details }
            }
            .padding(4)
        }
        .overlay(RoundedRectangle(cornerRadius: 8)
            .stroke(Color.accentColor, lineWidth: 2)
            .opacity(store.highlighted == profile.name ? 1 : 0)
            .animation(.easeInOut(duration: 0.3), value: store.highlighted))
        .id(profile.name)
    }

    var header: some View {
        HStack(spacing: 6) {
            Circle().fill(Color.named(profile.desktop?.color)).frame(width: 9, height: 9)
            Text(profile.name).font(.headline)
            Badge(text: profile.kind == "both" ? "Desktop + Code" : profile.kind == "desktop" ? "Desktop" : "Code")
            Spacer()
            IconButton(symbol: expanded ? "chevron.up" : "chevron.down", help: "Details") {
                expanded.toggle()
                if expanded { store.loadSizes() }
            }
            Menu {
                Button("Rename…", action: onRename)
                if profile.desktop != nil {
                    let selfUpdating = profile.desktop?.updates == "self"
                    Button(selfUpdating ? "Let the launcher keep it current" : "Let Claude update it itself") {
                        store.act("Updating…", ["self-update", profile.name, selfUpdating ? "off" : "on"])
                    }
                    Button("Repair launcher") {
                        store.act("Repairing…", ["repair", profile.name], done: "Launcher repaired")
                    }
                    Button("Show data folder") { NSWorkspace.shared.open(URL(fileURLWithPath: profile.desktop!.dataDir)) }
                }
                if let c = profile.code {
                    Button("Show config folder") { NSWorkspace.shared.open(URL(fileURLWithPath: c.configDir)) }
                }
                if profile.code?.account != nil {
                    Divider()
                    Button("Sign out of Claude Code (CLI)…") {
                        confirm("Sign \(profile.name)'s Claude Code out?",
                                "Its CLI signs out of \(accountLabel(profile.code?.account) ?? "its account"). Its folder, chats and settings stay, and the App part is not touched. Sign in again any time.",
                                button: "Sign Out") {
                            store.act("Signing out…", ["sign-out", profile.name], done: "Signed out \(profile.name)'s CLI")
                        }
                    }
                }
                Divider()
                if profile.desktop != nil && profile.code != nil {
                    Button("Remove only the App part…") { onRemovePart("desktop") }
                    Button("Remove only the CLI part…") { onRemovePart("code") }
                }
                Button("Remove…", role: .destructive, action: onRemove)
            } label: { Image(systemName: "ellipsis.circle") }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
        }
    }

    func desktop(_ d: DesktopHalf) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                KindTag(kind: merged ? .both : .app)
                Text(merged ? (accountLabel(profile.code?.account) ?? "") : desktopAccountLabel(d.account))
                    .font(.system(size: 12)).lineLimit(1).truncationMode(.middle)
                    .layoutPriority(1)
                if !merged { NameAccountButton(account: d.account) }
                Spacer(minLength: 4)
                if d.clone.running {
                    Button("Quit") {
                        store.act("Quitting…", ["quit-desktop", profile.name])
                    }.controlSize(.small).fixedSize()
                }
                Button(d.clone.running ? "Show" : d.account == nil ? "Sign in" : "Open") {
                    store.act("Opening \(profile.name)…", ["open-desktop", profile.name])
                }.controlSize(.small).fixedSize()
                RowMenu {
                    if merged { Section("App") { appActions(d) }; Section("CLI") { if let c = profile.code { cliActions(c) } } }
                    else { appActions(d) }
                }
            }
            // Stats on their own line: beside the buttons they squeeze the
            // account name and wrap.
            let sessions = merged ? (profile.code?.sessions.count ?? 0) : 0
            let fromApp = profile.code?.desktopSessions?.count ?? 0
            if d.clone.running || sessions > 0 || fromApp > 0 {
                HStack(spacing: 6) {
                    if d.clone.running {
                        Text("App \(memory(d.clone.memoryMB)) · \(String(format: "%.0f", d.clone.cpu))% CPU")
                            .font(.system(size: 10)).foregroundStyle(.secondary)
                    }
                    if sessions > 0 { Badge(text: "\(sessions) CLI running", tint: .green) }
                    if fromApp > 0 {
                        Text("· \(fromApp) Code session\(fromApp == 1 ? "" : "s") in the app")
                            .font(.system(size: 10)).foregroundStyle(.secondary)
                            .help("Claude Code sessions that Claude Desktop runs for its Code tab and tasks. You did not start them in a terminal.")
                    }
                }
            }
            if !d.clone.wrongAccountPids.isEmpty {
                Label("Running without its profile, so it shows your default account. Quit it and open it here.",
                      systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 11)).foregroundStyle(.orange)
            }
            if !d.launcherExists {
                Label("Its launcher is missing. Run Repair or doctor --fix.", systemImage: "exclamationmark.triangle")
                    .font(.system(size: 11)).foregroundStyle(.orange)
            }
        }
    }

    func code(_ c: CodeHalf) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            if !merged {
                HStack(spacing: 6) {
                    KindTag(kind: .cli)
                    Text(accountLabel(c.account) ?? "Not signed in").font(.system(size: 12)).lineLimit(1)
                    Spacer()
                    if !c.sessions.isEmpty { Badge(text: "\(c.sessions.count) running", tint: .green) }
                    if c.account == nil {
                        Button("Sign in") { store.signIn(profile.name) }.controlSize(.small)
                    }
                    RowMenu { cliActions(c) }
                }
            }
            if c.account != nil {
                UsageBars(usage: store.usage[profile.name]) { store.signIn(profile.name) }
                if let uuid = c.account?.accountUuid, let owner = store.firstOwner(of: uuid), owner != profile.name {
                    // Same numbers as another card: say which, briefly.
                    Label("Same account as \(owner) CLI, so the same usage",
                          systemImage: "link")
                        .font(.system(size: 10)).foregroundStyle(.secondary)
                }
            }
            CLIButtons(target: profile.name)
        }
    }


    @ViewBuilder func appActions(_ d: DesktopHalf) -> some View {
        Button(d.clone.running ? "Show" : "Open") { store.act("Opening \(profile.name)…", ["open-desktop", profile.name]) }
        if d.clone.running {
            Button("Quit") { store.act("Quitting…", ["quit-desktop", profile.name]) }
        }
        if let a = d.account, a.matched == nil {
            Button(a.nickname == nil ? "Name this account…" : "Rename this account…") { nameAccount(a, store: store) }
        }
        if d.account != nil {
            Button("Sign out…") {
                // Claude Desktop's sign-in is its own; this app cannot end it.
                confirm("Sign out in Claude Desktop",
                        "Claude opens for \(profile.name). Click your name at the bottom of its sidebar and choose Log out.",
                        button: "Open Claude", destructive: false) {
                    store.act("Opening \(profile.name)…", ["open-desktop", profile.name])
                }
            }
        }
        Divider()
        Button("Show data folder") { reveal(d.dataDir) }
        Button("Show launcher") { reveal(d.appPath) }
        Button("Repair launcher") { store.act("Repairing…", ["repair", profile.name], done: "Launcher repaired") }
        let selfUpdating = d.updates == "self"
        Button(selfUpdating ? "Let the launcher keep it current" : "Let Claude update it itself") {
            store.act("Updating…", ["self-update", profile.name, selfUpdating ? "off" : "on"])
        }
        if profile.code != nil {
            Divider()
            Button("Remove only the App part…", role: .destructive) { onRemovePart("desktop") }
        }
    }

    @ViewBuilder func cliActions(_ c: CodeHalf) -> some View {
        Button("Start Claude Code") { store.openCLI(profile.name, inFolder: false) }
        Button("Start in folder…") { store.openCLI(profile.name, inFolder: true) }
        Button("Copy command") { store.copyCommand(profile.name) }
        Divider()
        if c.account == nil {
            Button("Sign in…") { store.signIn(profile.name) }
        } else {
            Button("Sign out…") {
                confirm("Sign \(profile.name)'s Claude Code out?",
                        "Its CLI signs out of \(accountLabel(c.account) ?? "its account"). Its folder, chats and settings stay, and the App part is not touched. Sign in again any time.",
                        button: "Sign Out") {
                    store.act("Signing out…", ["sign-out", profile.name], done: "Signed out \(profile.name)'s CLI")
                }
            }
            Button("Switch account…") { store.signIn(profile.name) }
        }
        Divider()
        Button("Show config folder") { reveal(c.configDir) }
        if profile.desktop != nil {
            Divider()
            Button("Remove only the CLI part…", role: .destructive) { onRemovePart("code") }
        }
    }

    var details: some View {
        VStack(alignment: .leading, spacing: 3) {
            Divider()
            if let d = profile.desktop {
                row("Claude copy", cloneText(d.clone, installed: store.state?.claude.desktopApp?.version))
                row("Updates", updatesText(d.updates))
                if d.color != nil && !d.clone.colorIntact { row("Colour", "lost after an update; doctor --fix restores it") }
                row("Data", shortPath(d.dataDir) + (size?.desktopDataMB.map { " · \(memory($0))" } ?? ""))
                row("Copy size", size?.cloneMB.map { "\(memory($0)) (shared with Claude.app on APFS)" } ?? "…")
                row("Launcher", shortPath(d.appPath))
            }
            if let c = profile.code {
                row("Config", shortPath(c.configDir) + (size?.codeConfigMB.map { " · \(memory($0))" } ?? ""))
                row("Command", c.alias)
                if let gh = c.ghConfigDir { row("GitHub CLI", shortPath(gh)) }
                if let plan = c.account?.plan { row("Plan", plan) }
            }
            if !profile.createdAt.isEmpty { row("Created", String(profile.createdAt.prefix(10))) }
        }
        .font(.system(size: 11))
    }

    func row(_ k: String, _ v: String) -> some View {
        HStack(alignment: .top) {
            Text(k).foregroundStyle(.secondary).frame(width: 74, alignment: .leading)
            Text(v).textSelection(.enabled).lineLimit(2)
        }
    }

    func cloneText(_ c: Clone, installed: String?) -> String {
        guard c.exists else { return "missing (doctor --fix builds it)" }
        let v = c.version ?? "?"
        switch c.state {
        case "behind": return "\(v), updates to \(installed ?? "?") on next open"
        case "ahead": return "\(v), newer than installed \(installed ?? "?")"
        default: return "\(v), current"
        }
    }

    func updatesText(_ u: String) -> String {
        switch u {
        case "launcher": return "kept current by its launcher"
        case "self": return "Claude updates it itself"
        case "own-config": return "its own Claude configuration decides"
        default: return "not set up yet (doctor --fix)"
        }
    }
}

// MARK: - Add, rename, remove

struct AddView: View {
    @EnvironmentObject var store: Store
    var close: () -> Void
    @State private var name = ""
    @State private var targets = "both"
    @State private var color = "none"
    @State private var seedCode = false
    @State private var seedConnectors = false
    @State private var gh = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button { close() } label: { Image(systemName: "chevron.left") }.buttonStyle(.borderless)
                Text("Add an account").font(.headline)
            }
            TextField("Name, e.g. work or client-acme", text: $name)
                .textFieldStyle(.roundedBorder)
                .onChange(of: name) { typed in
                    let fixed = profileName(typed)
                    if fixed != typed { name = fixed }
                }
            Text("Lowercase letters, numbers and hyphens. Becomes the command claude-\(name.isEmpty ? "name" : name).")
                .font(.system(size: 10)).foregroundStyle(.secondary)
            Picker("Set up", selection: $targets) {
                Text("Both").tag("both")
                Text("Desktop").tag("desktop")
                Text("Code").tag("code")
            }.pickerStyle(.segmented)
            if targets != "code" {
                Picker("Dock colour", selection: $color) {
                    Text("None").tag("none")
                    ForEach(store.state?.colors ?? [], id: \.self) { c in
                        Label(c.capitalized, systemImage: "circle.fill").foregroundStyle(Color.named(c)).tag(c)
                    }
                }
                Toggle("Copy my default Desktop connectors (with their keys)", isOn: $seedConnectors)
            }
            if targets != "desktop" {
                Toggle("Copy my Claude Code setup (skills, settings, MCP, with their keys)", isOn: $seedCode)
                if store.state?.ghAvailable == true {
                    Toggle("Give it its own GitHub CLI login", isOn: $gh)
                }
            }
            Text("Nothing from using your default account comes along: no chats, history or sign-in.")
                .font(.system(size: 10)).foregroundStyle(.secondary)
            HStack {
                Spacer()
                Button("Cancel", action: close)
                Button("Create") {
                    var args = ["add", "--name", name.trimmingCharacters(in: CharacterSet(charactersIn: "-")), "--targets", targets]
                    if targets != "code" {
                        args += ["--color", color]
                        if seedConnectors { args.append("--seed-connectors") }
                    }
                    if targets != "desktop" {
                        if seedCode { args.append("--seed-code") }
                        if gh { args.append("--gh") }
                    }
                    store.act("Creating \(name)…", args, done: "Created \(name). Sign in on first open.") { close() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty || store.busy != nil)
            }
        }
        .toggleStyle(.checkbox)
        .font(.system(size: 12))
    }
}

struct RenameView: View {
    @EnvironmentObject var store: Store
    var profile: ProfileState
    var close: () -> Void
    @State private var name = ""
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button { close() } label: { Image(systemName: "chevron.left") }.buttonStyle(.borderless)
                Text("Rename \(profile.name)").font(.headline)
            }
            TextField("New name", text: $name)
                .textFieldStyle(.roundedBorder)
                .onChange(of: name) { typed in
                    let fixed = profileName(typed)
                    if fixed != typed { name = fixed }
                }
            if profile.code != nil {
                Text("Renaming signs its Claude Code half out: you run /login once more. Chats, skills and settings move with it.")
                    .font(.system(size: 11)).foregroundStyle(.secondary)
            }
            if profile.desktop?.clone.running == true {
                Text("Quit its Claude Desktop window first.").font(.system(size: 11)).foregroundStyle(.orange)
            }
            HStack {
                Spacer()
                Button("Cancel", action: close)
                Button("Rename") {
                    store.act("Renaming…", ["rename", profile.name, name.trimmingCharacters(in: CharacterSet(charactersIn: "-"))], done: "Renamed to \(name)") { close() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(name.isEmpty || store.busy != nil)
            }
        }
        .font(.system(size: 12))
    }
}

struct RemoveView: View {
    @EnvironmentObject var store: Store
    var profile: ProfileState
    /// "desktop" or "code" to remove only that part and keep the profile.
    var part: String? = nil
    var close: () -> Void
    // Removing cleans up by default; untick to keep a folder for later.
    @State private var deleteDesktop = true
    @State private var deleteCode = true
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button { close() } label: { Image(systemName: "chevron.left") }.buttonStyle(.borderless)
                Text(part == "desktop" ? "Remove the App part of \(profile.name)"
                     : part == "code" ? "Remove the CLI part of \(profile.name)" : "Remove \(profile.name)").font(.headline)
            }
            Text(part == "desktop" ? "Removes its Claude Desktop launcher and copy of Claude. Its Claude Code part stays."
                 : part == "code" ? "Removes its terminal command. Its Claude Desktop part stays, and its launcher stops pointing Desktop's Code tab at this folder."
                 : "Removes its launcher, its copy of Claude, its terminal command and its entry, plus the data ticked below.")
                .font(.system(size: 11)).foregroundStyle(.secondary)
            if let d = profile.desktop, part != "code" {
                Toggle("Also delete Desktop data (chats cached here, settings) at \(shortPath(d.dataDir))", isOn: $deleteDesktop)
            }
            if let c = profile.code, part != "desktop" {
                Toggle("Also delete Claude Code config (history, skills, settings) and its saved sign-in at \(shortPath(c.configDir))", isOn: $deleteCode)
            }
            if profile.desktop?.clone.running == true && part != "code" {
                Text("Quit its Claude Desktop window first.").font(.system(size: 11)).foregroundStyle(.orange)
            }
            HStack {
                Spacer()
                Button("Cancel", action: close)
                Button("Remove", role: .destructive) {
                    var args: [String]
                    let permanent: Bool
                    if let part {
                        let wipe = part == "desktop" ? deleteDesktop : deleteCode
                        args = ["remove-part", profile.name, part] + (wipe ? ["--delete-data"] : [])
                        permanent = wipe
                    } else {
                        args = ["remove", profile.name]
                        if deleteDesktop { args.append("--delete-desktop-data") }
                        if deleteCode { args.append("--delete-code-data") }
                        permanent = deleteDesktop || deleteCode
                    }
                    confirm("Remove \(profile.name)?",
                            permanent ? "The folders you ticked are deleted permanently." : "Its data folders are kept.",
                            button: "Remove") {
                        store.act("Removing…", args, done: "Removed \(profile.name)") { close() }
                    }
                }
                .disabled(store.busy != nil)
            }
        }
        .toggleStyle(.checkbox)
        .font(.system(size: 12))
    }
}

// MARK: - Panel

// MARK: - Running and Files

struct RunningView: View {
    @EnvironmentObject var store: Store
    var state: State

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            GroupBox {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Claude Desktop").font(.system(size: 12, weight: .semibold))
                    let open = desktops
                    if open.isEmpty {
                        Text("No Claude Desktop windows open.").font(.system(size: 11)).foregroundStyle(.secondary)
                    }
                    ForEach(open, id: \.name) { d in
                        HStack(spacing: 6) {
                            Circle().fill(Color.named(d.color)).frame(width: 8, height: 8)
                            Text(d.name == "default" ? "Default" : d.name).font(.system(size: 12, weight: .medium))
                            Text(d.who).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                            Spacer(minLength: 4)
                            Text("\(memory(d.memoryMB)) · \(String(format: "%.0f", d.cpu))%")
                                .font(.system(size: 10)).foregroundStyle(.secondary)
                            if d.name != "default" {
                                Button("Quit") { store.act("Quitting…", ["quit-desktop", d.name]) }.controlSize(.small)
                            }
                            Button("Show") {
                                store.act("Opening…", d.name == "default" ? ["open-default-desktop"] : ["open-desktop", d.name])
                            }.controlSize(.small)
                        }
                    }
                }.padding(4).frame(maxWidth: .infinity, alignment: .leading)
            }
            GroupBox {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Claude Code").font(.system(size: 12, weight: .semibold))
                    let groups = cli
                    if groups.isEmpty {
                        Text("No Claude Code sessions running.").font(.system(size: 11)).foregroundStyle(.secondary)
                    }
                    ForEach(groups, id: \.title) { g in
                        VStack(alignment: .leading, spacing: 3) {
                            HStack(spacing: 4) {
                                Text(g.title).font(.system(size: 11, weight: .medium))
                                if let who = g.who { Text(who).font(.system(size: 10)).foregroundStyle(.secondary).lineLimit(1) }
                            }
                            SessionsList(sessions: g.sessions)
                        }
                    }
                }.padding(4).frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    struct OpenDesktop { var name: String; var color: String?; var who: String; var memoryMB: Int; var cpu: Double }
    struct CLIGroup { var title: String; var who: String?; var sessions: [Session] }

    var desktops: [OpenDesktop] {
        var out: [OpenDesktop] = []
        if let d = state.default.desktop, d.running {
            out.append(.init(name: "default", color: nil, who: desktopAccountLabel(d.account), memoryMB: d.memoryMB, cpu: d.cpu))
        }
        for p in state.profiles {
            if let d = p.desktop, d.clone.running {
                out.append(.init(name: p.name, color: d.color, who: desktopAccountLabel(d.account),
                                 memoryMB: d.clone.memoryMB, cpu: d.clone.cpu))
            }
        }
        return out
    }

    var cli: [CLIGroup] {
        var out: [CLIGroup] = []
        if !state.default.code.sessions.isEmpty {
            out.append(.init(title: "Default", who: accountLabel(state.default.code.account), sessions: state.default.code.sessions))
        }
        for p in state.profiles {
            if let c = p.code, !c.sessions.isEmpty {
                out.append(.init(title: p.name, who: accountLabel(c.account), sessions: c.sessions))
            }
        }
        for o in state.otherSessions {
            out.append(.init(title: shortPath(o.configDir), who: "not one of your profiles", sessions: o.sessions))
        }
        return out
    }
}

struct FilesView: View {
    @EnvironmentObject var store: Store
    var state: State

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let total = total {
                Text("\(memory(total)) on disk in all").font(.system(size: 11)).foregroundStyle(.secondary)
            }
            GroupBox {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Default").font(.system(size: 12, weight: .semibold))
                    let size = store.sizes["default"]
                    if let d = state.default.desktop { row("Desktop data", d.dataDir, size?.desktopDataMB) }
                    row("Code config", state.default.code.configDir, size?.codeConfigMB)
                }.padding(4).frame(maxWidth: .infinity, alignment: .leading)
            }
            ForEach(state.profiles) { p in
                GroupBox {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 6) {
                            Circle().fill(Color.named(p.desktop?.color)).frame(width: 8, height: 8)
                            Text(p.name).font(.system(size: 12, weight: .semibold))
                        }
                        let size = store.sizes[p.name]
                        if let d = p.desktop {
                            row("Desktop data", d.dataDir, size?.desktopDataMB)
                            row("Claude copy", d.clone.path, size?.cloneMB, note: "shared with Claude.app")
                            row("Launcher", d.appPath, nil)
                        }
                        if let c = p.code {
                            row("Code config", c.configDir, size?.codeConfigMB)
                            if let gh = c.ghConfigDir { row("GitHub CLI", gh, nil) }
                        }
                    }.padding(4).frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    /// Everything but the Claude copies, which share their blocks with
    /// Claude.app and take next to nothing.
    var total: Int? {
        let all = store.sizes.values.flatMap { [$0.desktopDataMB, $0.codeConfigMB] }.compactMap { $0 }
        return all.isEmpty ? nil : all.reduce(0, +)
    }

    func row(_ label: String, _ path: String, _ mb: Int?, note: String? = nil) -> some View {
        HStack(spacing: 6) {
            Text(label).foregroundStyle(.secondary).frame(width: 82, alignment: .leading)
            Text(shortPath(path)).lineLimit(1).truncationMode(.middle).help(path)
            Spacer(minLength: 4)
            if let mb { Text(memory(mb)).foregroundStyle(.secondary).help(note ?? "") }
            IconButton(symbol: "folder", help: "Show in Finder") {
                NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
            }
        }
        .font(.system(size: 11))
    }
}

// MARK: - Import

struct ImportView: View {
    @EnvironmentObject var store: Store
    var close: () -> Void
    @State private var names: [String: String] = [:]

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Button { close() } label: { Image(systemName: "chevron.left") }.buttonStyle(.borderless)
                Text("Import existing accounts").font(.headline)
            }
            Text("Claude folders already on this Mac that no profile manages. Importing keeps them exactly as they are, sign-in, chats and settings included, and adds a launcher and a command.")
                .font(.system(size: 11)).foregroundStyle(.secondary)
            if store.candidates.isEmpty {
                Text("Nothing left to import.").font(.system(size: 12)).foregroundStyle(.secondary)
            }
            ForEach(store.candidates) { c in
                GroupBox {
                    VStack(alignment: .leading, spacing: 6) {
                        if let d = c.desktopDir {
                            HStack(spacing: 6) {
                                KindTag(kind: .app)
                                Text(shortPath(d)).lineLimit(1).truncationMode(.middle)
                                Spacer()
                                Text(c.desktopAccount.map { "Account \($0.prefix(8))" } ?? "not signed in").foregroundStyle(.secondary)
                            }
                        }
                        if let code = c.codeDir {
                            HStack(spacing: 6) {
                                KindTag(kind: .cli)
                                Text(shortPath(code)).lineLimit(1).truncationMode(.middle)
                                Spacer()
                                if c.running { Badge(text: "in use", tint: .green) }
                                Text(accountLabel(c.codeAccount) ?? "not signed in").foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                        HStack {
                            TextField("Profile name", text: Binding(
                                get: { names[c.id] ?? c.name },
                                set: { names[c.id] = profileName($0) }))
                                .textFieldStyle(.roundedBorder)
                            Button("Import") {
                                let name = (names[c.id] ?? c.name).trimmingCharacters(in: CharacterSet(charactersIn: "-"))
                                var args = ["import", "--name", name]
                                if let d = c.desktopDir { args += ["--desktop-dir", d] }
                                if let code = c.codeDir { args += ["--code-dir", code] }
                                store.act("Importing \(name)…", args, done: "Imported \(name)") { store.loadImportable() }
                            }
                            .disabled(store.busy != nil || (names[c.id] ?? c.name).isEmpty)
                        }
                    }
                    .font(.system(size: 11))
                    .padding(4)
                }
            }
        }
        .font(.system(size: 12))
    }
}

enum Tab: String, CaseIterable { case accounts = "Accounts", running = "Running", files = "Files" }

enum Screen: Equatable {
    case importing
    case main
    case add
    case rename(ProfileState)
    case remove(ProfileState)
    case removePart(ProfileState, String)
}

struct Panel: View {
    @EnvironmentObject var store: Store
    @State private var screen: Screen = .main
    @State private var launchAtLogin = SMAppService.mainApp.status == .enabled
    @State private var listHeight: CGFloat = 300
    @AppStorage("tab") private var tab: Tab = .accounts

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            switch screen {
            case .main: main
            case .add: AddView { screen = .main }
            case .importing: ImportView { screen = .main; store.loadImportable() }
            case .rename(let p): RenameView(profile: p) { screen = .main }
            case .remove(let p): RemoveView(profile: p) { screen = .main }
            case .removePart(let p, let part): RemoveView(profile: p, part: part) { screen = .main }
            }
            status
        }
        .padding(12)
        .frame(width: 400)
        .onAppear { store.start() }
        .onDisappear { store.stop() }
    }

    var main: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Claude Profiles").font(.title3.bold())
                Spacer()
                if let window = windowApp {
                    IconButton(symbol: "macwindow", help: "Open the Claude Profiles window") {
                        NSWorkspace.shared.openApplication(at: window, configuration: NSWorkspace.OpenConfiguration())
                    }
                }
                IconButton(symbol: "arrow.clockwise", help: "Refresh") { store.refresh(); store.loadUsage() }
                settingsMenu
            }
            if let s = store.state {
                versions(s)
                Picker("", selection: $tab) {
                    ForEach(Tab.allCases, id: \.self) { t in
                        Text(t == .running && runningCount(s) > 0 ? "Running (\(runningCount(s)))" : t.rawValue).tag(t)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .onChange(of: tab) { t in if t == .files { store.loadSizes() } }
                ScrollViewReader { proxy in
                ScrollView {
                    Group {
                        switch tab {
                        case .accounts:
                            VStack(spacing: 8) {
                                DefaultCard(state: s)
                                ForEach(s.profiles) { p in
                                    ProfileCard(profile: p, size: store.sizes[p.name],
                                                onRename: { screen = .rename(p) }, onRemove: { screen = .remove(p) },
                                                onRemovePart: { screen = .removePart(p, $0) })
                                }
                            }
                        case .running: RunningView(state: s)
                        case .files: FilesView(state: s)
                        }
                    }
                    .background(GeometryReader { g in
                        Color.clear.preference(key: ListHeight.self, value: g.size.height)
                    })
                }
                .onChange(of: store.scrollTarget) { target in
                    guard let target else { return }
                    withAnimation { proxy.scrollTo(target, anchor: .top) }
                    store.scrollTarget = nil
                }
                }
                // A menu bar window sizes to its content, and a ScrollView has
                // none of its own: without a height it collapses to nothing.
                .frame(height: min(max(listHeight, 60), 520))
                .onPreferenceChange(ListHeight.self) { listHeight = $0 }
                if tab == .accounts {
                    HStack(spacing: 8) {
                        Button { screen = .add } label: {
                            Label("Add an account", systemImage: "plus.circle.fill").frame(maxWidth: .infinity)
                        }
                        if !store.candidates.isEmpty {
                            Button { screen = .importing } label: {
                                Label("Import \(store.candidates.count)", systemImage: "square.and.arrow.down").frame(maxWidth: .infinity)
                            }
                            .help("Make profiles from Claude accounts already on this Mac")
                        }
                    }
                    .controlSize(.large)
                }
            } else if store.error == nil {
                ProgressView().frame(maxWidth: .infinity)
            }
        }
    }

    /// The window app this panel ships inside (Claude Profiles.app/Contents/
    /// Helpers/), when it runs as that helper rather than on its own.
    var windowApp: URL? {
        let outer = Bundle.main.bundleURL.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return outer.pathExtension == "app" && Bundle.main.bundleURL.deletingLastPathComponent().lastPathComponent == "Helpers" ? outer : nil
    }

    func runningCount(_ s: State) -> Int {
        var n = s.default.code.sessions.count + (s.default.desktop?.running == true ? 1 : 0)
        for p in s.profiles {
            n += (p.code?.sessions.count ?? 0) + (p.desktop?.clone.running == true ? 1 : 0)
        }
        return n + s.otherSessions.reduce(0) { $0 + $1.sessions.count }
    }

    func versions(_ s: State) -> some View {
        HStack(spacing: 6) {
            if let d = s.claude.desktopApp { Badge(text: "Desktop \(d.version ?? "?")") }
            if let c = s.claude.cli {
                Badge(text: "Code \(c.version?.split(separator: " ").first.map(String.init) ?? "?")",
                      tint: c.shadowed.isEmpty ? .secondary : .orange)
            } else {
                Badge(text: "Code not on PATH", tint: .orange)
            }
            if s.helper != "current" && !s.profiles.filter({ $0.desktop != nil }).isEmpty {
                Badge(text: "run doctor --fix", tint: .orange)
            }
            Spacer()
            Text("v\(s.version)").font(.system(size: 10)).foregroundStyle(.tertiary)
        }
    }

    var settingsMenu: some View {
        Menu {
            Button("Check everything (doctor)…") {
                store.act("Opening doctor…", ["in-terminal", "doctor", "--terminal", store.terminal])
            }
            Button("Repair what doctor finds (doctor --fix)") {
                store.act("Running doctor --fix…", ["doctor-fix"], done: "doctor --fix finished")
            }
            Button("Copy extensions between profiles…") {
                store.act("Opening…", ["in-terminal", "extensions", "--terminal", store.terminal])
            }
            Button("Full add wizard (custom folders, linking)…") {
                store.act("Opening…", ["in-terminal", "add", "--terminal", store.terminal])
            }
            Divider()
            Picker("Open Claude Code in", selection: $store.terminal) {
                ForEach(["Terminal", "iTerm", "Ghostty", "Warp"], id: \.self) { Text($0).tag($0) }
            }
            Toggle("Open at login", isOn: Binding(get: { launchAtLogin }, set: { on in
                do {
                    if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
                    launchAtLogin = on
                } catch {
                    store.error = "Could not change login item: \(error.localizedDescription)"
                }
            }))
            Divider()
            Button("Quit Claude Profiles") { NSApp.terminate(nil) }
        } label: { Image(systemName: "gearshape") }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
    }

    @ViewBuilder var status: some View {
        if let b = store.busy {
            HStack(spacing: 6) { ProgressView().controlSize(.small); Text(b).font(.system(size: 11)) }
        } else if let e = store.error {
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
                Text(e).font(.system(size: 11)).textSelection(.enabled)
                Spacer()
                IconButton(symbol: "xmark", help: "Dismiss") { store.error = nil }
            }
        } else if let n = store.notice {
            Label(n, systemImage: "checkmark.circle.fill").font(.system(size: 11)).foregroundStyle(.green)
        }
    }
}

struct ListHeight: PreferenceKey {
    static var defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = max(value, nextValue()) }
}

// MARK: - App

@main
struct ClaudeProfilesApp: App {
    @StateObject private var store = Store()

    init() {
        // After a reinstall, bring the launchers' helper level before anyone
        // clicks a Dock icon, not only once the panel is opened.
        // Also gives Claude Code's URL handler app an icon again after a
        // Claude Code update recreates it without one.
        Task.detached { _ = try? await CLI.run(["startup"]) }
    }

    var body: some Scene {
        MenuBarExtra {
            Panel().environmentObject(store)
        } label: {
            Image(nsImage: ProfilesIcon.menuBar())
        }
        .menuBarExtraStyle(.window)
    }
}
