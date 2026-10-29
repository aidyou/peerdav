[English](README.md) | [简体中文](READMD-zh.md)

# PeerDav

## Work with remote files like they're on your own computer

PeerDav is a desktop app for secure file sharing between authorized computers on a local network. Share a folder from your computer, or browse and manage files on another computer with the permissions its owner gives you.

Every copy of PeerDav is both a client and a server. Your desktop can share project files with your laptop while you use the same app to browse the laptop's shared folders.

## Download and install

[Download PeerDav from GitHub Releases](https://github.com/aidyou/peerdav/releases)

Open the release page and choose the package for your operating system and processor architecture. The steps below apply if your system blocks the downloaded app. Only proceed with a package you downloaded from this project's release page and trust.

### macOS

Install PeerDav in **Applications**. If macOS blocks it with a message such as “developer cannot be verified” or “app is damaged,” first check that the download is complete and comes from the release page above. For a trusted copy, open Terminal and remove the quarantine attribute from this app only:

```sh
xattr -dr com.apple.quarantine "/Applications/peerdav.app"
```

Then open PeerDav again. Adjust the path if you installed it elsewhere. This command removes the downloaded-file quarantine flag; it does not repair a damaged download. If macOS reports malware, do not bypass that warning. Do not disable Gatekeeper system-wide.

### Windows

Run the downloaded installer. If Microsoft Defender SmartScreen displays **Windows protected your PC**, check that the filename and download source match the release you chose. For a trusted installer, select **More info**, then **Run anyway** if that option is available.

If a work or school policy blocks installation, contact your administrator. If antivirus software reports malware, stop and verify the package rather than disabling protection. When Windows Firewall asks about network access, allow it only on the trusted network profiles you intend to use for sharing.

## Choose what to share and who can access it

Select the local folders you want to share and set their read, write, and delete permissions separately. Another computer requests access, and you approve it before it can use your shares. Shared folders also have allow and deny rules for IP addresses, subnets, and MAC addresses. Deny rules take priority.

You can revoke a computer's authorization when it no longer needs access. Sharing one directory does not give it access to your entire computer.

PeerDav uses HTTPS for file transfers and checks device fingerprints to identify peers. On the first connection, use a trusted network or enter a fingerprint you have verified with the other computer's owner.

## Browse, transfer, and edit in one place

Open a computer you have permission to access and browse its shared folders. Choose a list, icon, column, or gallery view to suit the files you're working with.

Within your granted permissions, you can:

- Preview supported file types before downloading them.
- Upload and download files or whole folders, with transfer progress and cancellation controls.
- Open a file in a local application, edit it, then click **Upload changes** to send the saved version back.
- Delete remote files and folders when you have delete permission.

For example, keep your working documents on a desktop and use your laptop to find and edit a file. When you're done, upload the changes back to the desktop.

External editing downloads a local copy first; it does not mount the remote folder as a local drive. Remote deletions affect the shared files, so check what you've selected before deleting.

## Use your physical or virtual LAN

PeerDav works between computers that can reach each other on a local network, such as a home Wi-Fi network or an office Ethernet network.

That network can also be a virtual LAN built with tools such as [Tailscale](https://tailscale.com/) or [EasyTier](https://easytier.cn/). Once the computers can reach each other through that network, connect using the other computer's virtual-network address. PeerDav still handles device authorization and shared-folder permissions.

Local-network discovery helps you find nearby peers. Broadcast discovery may not cross virtual networks or subnets; if a computer does not appear, enter its HTTPS address manually, for example `https://192.168.1.20:4918`. Use the configured port if you have changed it.

Make sure the service listens on an address reachable by the other computer, and that your firewall and virtual-network access rules allow the connection. Tailscale and EasyTier are configured separately from PeerDav.

## Share your first folder

1. Run PeerDav on both computers. On first launch, choose a key-file location and create an administrator account. Back up the key file and keep the recovery code somewhere safe.
2. On the computer with the files, add a shared folder in settings. Set its permissions and allow rules, then start the WebDAV service on the appropriate network address.
3. On the other computer, select the discovered peer or enter its HTTPS address and request access.
4. Approve the request on the sharing computer. The authorized computer can then browse the shares its access rules permit.

To share in the other direction, repeat the folder setup and approval on the other computer. Each computer controls access to its own files.

The sharing computer must stay online with PeerDav and its service running. Locking the interface leaves background sharing and transfers running; quitting PeerDav stops the service.

## Build from source

PeerDav uses Tauri 2, Rust, Vue 3, and SQLite. The interface is available in English and Simplified Chinese.

Install Node.js compatible with Vite 8, pnpm, a Rust toolchain supporting edition 2024, and the [Tauri prerequisites for your operating system](https://v2.tauri.app/start/prerequisites/).

From the repository root:

```sh
pnpm install
pnpm tauri dev
```

Build a desktop package:

```sh
pnpm tauri build
```

Run the frontend and Rust tests:

```sh
pnpm test
cargo test --manifest-path src-tauri/Cargo.toml
```
