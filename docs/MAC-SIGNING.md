# Mac signing and notarization

Without a Developer ID signature and Apple's notarization, macOS blocks a downloaded Rowglow
("Apple could not verify Rowglow is free of malware"); people have to dig into System Settings →
Privacy & Security → Open Anyway. Signed and notarized, it opens like any other app, and macOS
stops asking about saved passwords after each update (the keychain recognises the same signer).

The build side is done: `scripts/release-mac.sh` builds, signs, notarizes and staples the `.app`
and the `.dmg`, and the GitHub workflow does the same when the secrets below are set. What's left
are the Apple account steps, which only you can do.

## 1. Join the Apple Developer Program ($99/year)

[developer.apple.com/programs/enroll](https://developer.apple.com/programs/enroll/), signed in with
your Apple ID.

- **Individual**: quickest; the certificate and the "identified developer" shown on the
  Mac carry your personal name.
- **Organization**: shows the company name; needs a D-U-N-S number for the company (free,
  can take a few days).

Approval usually takes a day or two. An "Apple Development" certificate from a free Xcode account
can't be used: it only runs apps on your own Macs.

## 2. Create the Developer ID Application certificate

Only the team's Account Holder can create it.

1. Xcode → Settings → Accounts → select the team → **Manage Certificates…** → **+** →
   **Developer ID Application**. Xcode creates the private key and installs the certificate in
   your login keychain.
2. Check: `security find-identity -v -p codesigning` lists
   `Developer ID Application: Your Name (ABCDE12345)` (the 10 characters are your team ID).
3. **Back it up**: Keychain Access → My Certificates → right-click it → Export → `.p12` with a
   strong password. Store the file and password in your password manager. You need it for CI
   and for another Mac. If you lose the private key you must create a new certificate.

## 3. Notarization credentials (pick one)

**A. App Store Connect API key (recommended, and what CI uses)**

1. [App Store Connect](https://appstoreconnect.apple.com) → Users and Access → Integrations →
   App Store Connect API → **Team Keys** → **+**, access **Developer**.
2. Note the **Key ID** and the **Issuer ID** (top of the page).
3. Download `AuthKey_<KEYID>.p8`. Apple lets you download it only once.
4. Put it where the tools look:
   ```bash
   mkdir -p ~/.appstoreconnect/private_keys
   mv ~/Downloads/AuthKey_*.p8 ~/.appstoreconnect/private_keys/
   chmod 600 ~/.appstoreconnect/private_keys/AuthKey_*.p8
   ```

**B. Apple ID + app-specific password**

1. [account.apple.com](https://account.apple.com) → Sign-In and Security → App-Specific
   Passwords → generate one named "Rowglow notarization".
2. Store it in your keychain (it prompts for the password, so it never lands in shell history):
   ```bash
   security add-generic-password -s rowglow-notary -a you@example.com -w
   ```

## 4. Configure and check

```bash
cp scripts/release-mac.env.example scripts/release-mac.env   # then fill it in
scripts/release-mac.sh --check
```

`release-mac.env` holds no secrets (it's git-ignored anyway): the website URL, optionally the
certificate name, and either the API key ID + issuer ID or your Apple ID + team ID. `--check`
builds nothing and calls no server; it lists what's still missing.

## 5. Build a release

```bash
scripts/release-mac.sh                                  # this Mac's chip (Apple Silicon → aarch64)
scripts/release-mac.sh --target x86_64-apple-darwin     # Intel
scripts/release-mac.sh --target universal-apple-darwin  # one DMG for both
```

What it does:

1. Checks the setup first, so a 5-minute build never fails at the end.
2. Builds with the Tauri bundler, which signs the app with your Developer ID (hardened runtime),
   sends it to Apple for notarization, staples the ticket and wraps it in a signed `.dmg`.
3. Notarizes the `.dmg` too and staples it, so Gatekeeper accepts the download itself, even offline.
4. Verifies the result: signature, Developer ID, hardened runtime, timestamp, stapled tickets and
   Gatekeeper's verdict for the DMG and for the app inside it.

Notarization takes 1–10 minutes per upload. If Apple rejects a build, the script prints Apple's
log with the reason.

Intel and universal builds need Rust from rustup (Homebrew's Rust only builds for the Mac
it runs on). Switch with `brew uninstall rust && brew install rustup && rustup-init`, then
`rustup target add x86_64-apple-darwin`. Or let CI build Intel (below).

Check any build, or a downloaded copy, at any time:

```bash
scripts/release-mac.sh --verify path/to/Rowglow_1.2.0_aarch64.dmg
```

## 6. CI (GitHub Actions)

`.github/workflows/desktop-release.yml` signs and notarizes both Mac builds (Apple Silicon and
Intel) when these repository secrets are set (Settings → Secrets and variables → Actions):

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | The `.p12` from step 2, base64: `base64 -i DeveloperID.p12 \| gh secret set APPLE_CERTIFICATE` |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12` export password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Your Name (ABCDE12345)` |
| `APPLE_API_KEY` | The API key ID |
| `APPLE_API_ISSUER` | The issuer ID |
| `APPLE_API_KEY_P8` | The `.p8` file's contents: `gh secret set APPLE_API_KEY_P8 < AuthKey_<KEYID>.p8` |

Or, instead of the three API key secrets: `APPLE_ID`, `APPLE_PASSWORD` (the app-specific
password) and `APPLE_TEAM_ID`. Without any of them, CI builds ad-hoc signed DMGs as before.

The workflow stops if only some of the three certificate secrets are set, and warns if it can
sign but not notarize. A signed app that isn't notarized is still blocked.

## Good to know

- **The first signed build asks once per saved password.** Copies built before signing had a
  different signature, so macOS asks whether Rowglow may use each saved password it stored
  (enter the Mac password, click **Always Allow**). Later updates signed with the same
  certificate don't ask.
- **Renewal**: Developer ID certificates last 5 years. Apps signed before the expiry keep
  working because each signature carries a secure timestamp. Renew the membership yearly to
  keep notarizing new versions.
- **Local Network permission**: on macOS 15+, connecting to a database at a LAN address
  (192.168.x.x) shows a one-time "find devices on your local network" prompt with the reason
  from `apps/desktop/src-tauri/Info.plist`. `localhost` doesn't trigger it.
- **Windows** has its own warning (SmartScreen) until installers are signed with a code-signing
  certificate. See the README.
