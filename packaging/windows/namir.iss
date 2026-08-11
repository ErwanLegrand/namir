; namir.iss -- the Windows installer, per D-18.3 (release and packaging pipeline) and D-13.3
; (CLAP install paths). Compile with Inno Setup 6.3 or newer:
;
;     iscc /DAppVersion=1.2.3 /DVersionInfoVersion=1.2.3.0 packaging\windows\namir.iss
;
; See packaging/windows/README.md for the local build recipe, the ZIP command that ships beside
; this installer for FR-PKG-050, and the list of things that are NOT verified by any test.
;
; ---------------------------------------------------------------------------------------------
; THE BINARIES THIS INSTALLER SHIPS ARE UNSIGNED. This is risk R-11 (02-architecture.md Section
; 22), recorded here because this file is where a reader will look for it. On Windows that means:
;
;   * SmartScreen shows "Windows protected your PC" on every release. Reputation is what silences
;     it, and a low-volume unsigned publisher never accrues any, so this does not improve on its
;     own with time.
;   * Smart App Control, on machines that have it enabled, BLOCKS an unsigned installer outright
;     rather than warning. There is no user-visible override in that case.
;
; Do not work around this by telling users to disable a security feature. The fix is a signing
; identity; D-18.3's signing-conditional structure (the SignTool block below) is what keeps the
; cost of adopting one low -- it does not reduce the exposure until then. Revisit before any
; release aimed at non-developers.
; ---------------------------------------------------------------------------------------------

#define AppName        "Namir"
#define AppPublisher   "Erwan Patrick Legrand"
#define AppUrl         "https://github.com/ErwanLegrand/namir"
#define AppExe         "namir.exe"
#define ClapArtifact   "Namir.clap"

; The version comes from the release tag, not from a manifest: [workspace.package] in the root
; Cargo.toml carries no `version` key today (only `edition`, `license` and `rust-version`), and
; the two crates that produce these artifacts each carry their own `version = "0.1.0"`. The
; release workflow passes /DAppVersion=<tag without the leading v>. The default below exists so
; that a developer can compile this file locally without inventing a version number, and is
; deliberately not a plausible release version.
#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif

; VersionInfoVersion is the Win32 VERSIONINFO resource stamped into the setup executable and must
; be numeric (a.b.c.d), which AppVersion need not be -- a prerelease tag such as `0.2.0-rc1` is a
; perfectly good AppVersion and not a valid VersionInfoVersion. Passed separately rather than
; derived, so a prerelease never turns a version string into a compile error.
#ifndef VersionInfoVersion
  #define VersionInfoVersion "0.0.0.0"
#endif

; The staging tree `xtask bundle` produces -- and the ONLY thing this installer reads. Every file
; below comes from `target/bundle/windows`, which `cargo run -p xtask -- bundle --check` has
; already asserted (xtask/src/bundle.rs's `check`), so what ships is what that check passed on.
; Never add a Source: line pointing anywhere else: a file reaching the installer without passing
; through `bundle` is a file no check has ever seen.
;
; Default: <repo>/target/bundle/windows, resolved relative to this script. Override with
; /DStaging=<path> when CARGO_TARGET_DIR is set elsewhere (bundle honours that variable too).
; (The leading backslash on each is harmless whether or not ISPP's SourcePath already ends in one
; -- Win32 collapses a repeated separator in the middle of a path -- which is why it is spelled
; this way rather than assuming either form.)
#ifndef Staging
  #define Staging SourcePath + "\..\..\target\bundle\windows"
#endif

#ifndef OutputDir
  #define OutputDir SourcePath + "\..\..\target\dist"
#endif

; One guard, with the remedy line `xtask bundle` itself prints. Inno's own compiler already fails
; on each missing [Files] source, so this is not about correctness -- it is so that the first
; error a developer sees says "you have not run bundle" instead of "source file does not exist".
#if !FileExists(Staging + "\" + ClapArtifact)
  #error "No staging tree. Run: cargo build --release --workspace, then cargo run -p xtask -- bundle"
#endif

[Setup]
; Permanent identity of this product on Windows, and the registry key its uninstall entry lives
; under. It must never change: changing it makes an upgrade install a second copy alongside the
; first instead of replacing it. This is the Windows counterpart of the macOS bundle's
; CFBundleIdentifier (`org.legrand.namir`, xtask/src/bundle.rs's BUNDLE_IDENTIFIER); Inno's own
; convention is a GUID rather than a reverse-DNS string, so the two spellings differ by platform
; convention while naming the same product.
AppId={{AFF2C7FF-3B97-4551-8F4F-617B73E9D436}
AppName={#AppName}
AppVersion={#AppVersion}
; No AppVerName: it is deprecated in Inno 6 and superseded by AppName + AppVersion above, which
; together produce the same "Namir 1.2.3" wherever the wizard used to read it.
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
; The code is MIT OR Apache-2.0; the name "Namir" and the brand assets are not (NFR-LIC-070,
; TRADEMARK.md). Both halves are stated because a permissive code licence otherwise invites the
; inference that the mark came with it. No LicenseFile is set deliberately: the wizard's licence
; page shows exactly one document, and showing either LICENSE-MIT or LICENSE-APACHE alone would
; misstate a dual licence in which the choice is the user's. Both full texts are installed
; instead, unconditionally, which is what FR-PKG-040 actually requires.
AppCopyright=Copyright (c) 2026 {#AppPublisher}. Code: MIT OR Apache-2.0. The name and brand assets are not covered by that licence -- see TRADEMARK.md.
VersionInfoVersion={#VersionInfoVersion}
VersionInfoProductName={#AppName}
VersionInfoCompany={#AppPublisher}

; FR-PKG-030, the whole requirement, in the four directives below.
;
;   PrivilegesRequired=lowest                  -- Setup does not request elevation, so the default
;                                                 is the per-user scope. D-13.3's rationale asks
;                                                 for exactly this: "installation needs no
;                                                 administrator rights, which matters for users
;                                                 without them."
;   PrivilegesRequiredOverridesAllowed=dialog  -- and this is what makes the OTHER scope reachable.
;                                                 With `lowest` alone there is no system-wide
;                                                 option at all: Setup would simply never elevate.
;                                                 `dialog` puts Inno's install-mode page first,
;                                                 offering "Install for all users" (which restarts
;                                                 Setup elevated) or "Install for me only".
;                                                 `commandline` additionally lets the release
;                                                 pipeline or a scripted install pass /ALLUSERS or
;                                                 /CURRENTUSER non-interactively, which is how the
;                                                 FR-PKG-030 manual test exercises both scopes
;                                                 without a human clicking twice.
;
; D-18.3 names only PrivilegesRequired=lowest and describes the behaviour as escalating "only if
; the user asks". The second directive is what supplies the asking; see README.md's "What differs
; from D-18.3 as written".
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline

; Why 64-bit install mode is not optional here, and is the difference between installing to
; D-13.3's system-wide cell and installing to the wrong directory entirely:
;
; {autocf} is {usercf} when Setup runs non-elevated and {commoncf} when it runs elevated. {usercf}
; is %LOCALAPPDATA%\Programs\Common -- D-13.3's per-user cell, unconditionally. {commoncf},
; however, is the *32-bit* Common Files directory (C:\Program Files (x86)\Common Files) UNLESS
; Setup is running in 64-bit install mode, in which case it is C:\Program Files\Common Files --
; which is what %COMMONPROGRAMFILES% expands to on 64-bit Windows, and the only one of the two
; that CLAP's entry.h lists as a search path. Without the line below, the elevated install would
; put Namir.clap somewhere no host scans, and it would fail the way D-13.3 warns about: silently,
; with the plugin simply never appearing.
;
; x64compatible rather than x64os so that an ARM64 Windows 11 machine, which runs x64 binaries
; under emulation, is allowed to install them. It requires Inno Setup 6.3 or newer; on 6.2 and
; earlier the spelling is `x64`. A too-old Inno therefore fails loudly at compile time rather
; than quietly selecting a different architecture, which is the failure mode to prefer.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

; The FRS's platform table (Section 1.4) commits to Windows 11 x86-64 for 1.0. That is a support
; commitment, not a technical floor -- nothing in either product refuses Windows 10 -- so this
; blocks neither. 10.0 admits Windows 10 and 11; raising it to 10.0.22000 would be a distribution
; decision no requirement has taken.
MinVersion=10.0

DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
UninstallDisplayName={#AppName} {#AppVersion}
UninstallDisplayIcon={app}\{#AppExe}
OutputDir={#OutputDir}
OutputBaseFilename=namir-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Restart Manager, on by default, is what makes an upgrade over a running DAW survivable: it
; detects that a host process holds Namir.clap or that namir.exe is running, and asks the user to
; close them rather than failing mid-copy or (worse) leaving a half-replaced plugin. Named here
; rather than left implicit because a plugin file held open by a host is the ordinary case for
; this product, not an edge case.
CloseApplications=yes
RestartApplications=no

; FR-UI-110's executable-icon clause, M13. `images/namir.ico` is a *generated* artifact -- `xtask
; identity --write` renders it from images/namir.png and plain `xtask identity` byte-compares it,
; the same freshness gate M12's brand-mark blob runs under. It is not hand-committed artwork.
;
; This line covers the Setup executable only. The installed `namir.exe` carries its own PE resource,
; embedded post-build by `rcedit` before `xtask bundle` stages it -- see README.md's recipe. That
; split is D-17.3: the icon is embedded by the packaging pipeline rather than by a build script in a
; shipped crate, which is why a `cargo build` and a released binary differ in a user-visible way.
SetupIconFile={#SourcePath}\..\..\images\namir.ico

; D-18.3: "signing steps are skipped when the identity secret is absent and the unsigned build
; takes the identical code path, so enabling signing later is adding a secret, not restructuring."
; Passing /DSignToolName=<name> (with a matching /S<name>=... defining the tool) turns signing on
; without editing this file; passing nothing leaves the unsigned path -- the one exercised on
; every run today -- byte-for-byte unchanged.
#ifdef SignToolName
SignTool={#SignToolName}
SignedUninstaller=yes
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Types]
Name: "full"; Description: "Plugin and standalone application"
Name: "custom"; Description: "Custom installation"; Flags: iscustom

[Components]
; Both default to installed. They are separable because FR-CFG-030 requires each product to work
; without the other ("the standalone application shall not require the CLAP plugin to be
; installed, and the CLAP plugin shall not require the standalone application to be installed"),
; and its Verify method is "each is installed alone into a clean environment and exercised" --
; which needs an installer that can install each alone.
Name: "plugin"; Description: "CLAP plugin ({#ClapArtifact})"; Types: full custom
Name: "standalone"; Description: "Standalone application ({#AppExe})"; Types: full custom

[Files]
; FR-PKG-030's placement clause: {autocf}\CLAP is %COMMONPROGRAMFILES%\CLAP elevated and
; %LOCALAPPDATA%\Programs\Common\CLAP not -- D-13.3's Windows row, both cells, from this one line.
; See the ArchitecturesInstallIn64BitMode comment above for what makes the elevated half correct.
Source: "{#Staging}\{#ClapArtifact}"; DestDir: "{autocf}\CLAP"; Flags: ignoreversion; Components: plugin

Source: "{#Staging}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion; Components: standalone

; FR-PKG-040: the machine-generated attribution file of NFR-LIC-030 and the full text of both
; licences of NFR-LIC-010, in every distribution. No Components: parameter on any of the four, so
; they are installed whatever the user selects -- a plugin-only install still carries them, which
; is what "every distribution" means. README.md and TRADEMARK.md are staged by `xtask bundle`
; beside them and installed for the reasons that tool's own constants give: a distribution with no
; statement of what it is is a worse artifact than one with a redundant file in it, and the shipped
; binaries carry the brand mark, so NFR-LIC-070's statement of the mark's terms has to travel with
; them -- the README's licence section points at TRADEMARK.md, and a distribution that names a file
; and then omits it is worse than one that says nothing.
Source: "{#Staging}\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Staging}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Staging}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Staging}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Staging}\TRADEMARK.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; {autoprograms} follows the install scope the same way {autocf} does: the current user's Start
; menu for a per-user install, the all-users Start menu for a system-wide one.
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Components: standalone
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon; Components: standalone

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked; Components: standalone

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent; Components: standalone

; Uninstall is Inno's own, and it removes exactly what was installed, from the scope it was
; installed to: every [Files] entry is logged as it is copied and deleted in reverse at uninstall,
; and the uninstall registry entry is written under HKCU for a per-user install and HKLM for a
; system-wide one, so the two scopes uninstall independently. Two properties worth stating because
; they are easy to assume wrongly:
;
;   * {autocf}\CLAP is shared with every other CLAP vendor. Inno removes a directory it created
;     only if it is empty at uninstall time, so uninstalling Namir never takes another vendor's
;     plugin with it, and never removes a CLAP directory that existed before this installer ran.
;   * There is no [UninstallDelete] section, deliberately. Namir's settings and library index live
;     under %APPDATA%\Namir (namir-platform's config_dir), which the installer never wrote and
;     therefore has no business deleting; leaving them means reinstalling restores a working setup
;     rather than a blank one.
;
; Known limitation, Inno's and not this script's: a per-user install and a system-wide install are
; separate registrations, so a user who installs per-user and later reruns the installer elevated
; ends up with two copies. UsePreviousPrivileges (on by default) makes a rerun default to the mode
; last used, which is what keeps this from happening by accident.
