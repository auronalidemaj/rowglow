# Security

Rowglow holds database passwords and connects to production systems, so security reports get priority.

**Please don't open a public issue for a vulnerability.** Use GitHub's private reporting instead: the repository's **Security** tab → **Report a vulnerability**. Include the version, your OS and the steps to reproduce.

You'll get a reply within a few days. Fixes ship in the next release, with credit to you if you'd like it.

In scope: the desktop app in this repository (Rust core, UI, CI and release scripts). How it stores secrets: OS keychain first (macOS Keychain, Windows Credential Manager, Linux Secret Service); if no keychain is available, the app's own database in a folder readable only by the user, and the user is told.
