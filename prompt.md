# Original Project Prompt

> This file preserves the original request that initiated the RabbitHole project,
> saved verbatim for reference.

---

Create a client/server system reminiscent of BBSes, Hotline, Haxial, HDX, AOL, etc with full support for all platforms (desktop: macOS, Windows, Linux - mobile: iOS, iPadOS, android) where the server can be hosted anywhere (internal, cloud, docker, etc), the client provides visual elements. There should be CLI, TUI, embedded web, and native graphical interfacing for the client, and CLI, TUI, embedded web admin, and native graphical interfaces for the server.

Core features:
- users (register, login, profile information, avatar, banner image for live users connected list)
- files (ability to upload and download files, organized in folder trees) with full support for icons and other metadata. Also, the ability for connected clients to be able to list files without outright uploading them, so they can be swarm downloaded by other clients directly (similar to torrent but custom), so if the same file exists on other connected clients (as well as the server or other connected servers) chunks can be downloaded from multiple at a time. File bases, sub folders or individual files should allow for permission model
- cross server searching
- server directory and discovery
- message bases
- request system (desired files, message base, etc)
- support for ASCII and ANSI art
- built in server support for telnet, fingerd, who
- audio streaming for optional (server setting) radio station feature that lets connected users vote on what to play and the server streams it to clients that have it enabled in their settings
- welcome message and logo on client connect to server set by server admins
- server settings should be manageable by authorized users from the client and the server CLI/TUI/web
- support for NNTP Usenet syndication
- support for QWK mail reader syndication
- support for FidoNet syndication
- support for the ability for clients to be able to batch download messages in specific bases and still be able to read and reply while offline and will sync when next connected (offline mode)
- support for the ability to queue file transfers that will persist and resume when interrupted and later reconnected
- ability to direct message other users (with notifications), including the ability to attach files (max file size for attachments on server configurable by authorized users)
- different user levels (guest, user, moderator, admin, superuser) with guest (anonymous) optionally disabled for private servers
- use Rust (and likely Tauri for mobile/desktop platforms), putting the key functionality in rust for readability across CLI, TUI, web and native modes

Make sure to thoroughly research Haxial, HDX, Hotline, BBS software, AOL, etc in order to solidly understand the concept and fill meaningful features that might not be defined here.
The UI should be clean, minimal, but robust.
Make a detailed plan first in markdown file, making considerations and identifying as many details upfront as possible, and then distill it out into a todo markdown file with checkmarks for us to keep track of implementation. Don’t start actual implementation until I’ve reviewed this in detail.
Ask me for any clarifications or questions using the AskUserQuestion tool (I am happy to make many rounds of this if you need me to).
Also, it’s important that we build the plan thoroughly and not leave features out. It can be organized in waves and using a dependency structure to ensure we’re building things in the correct order.
Also, let’s use ultramode.
Save this prompt to prompt.md
