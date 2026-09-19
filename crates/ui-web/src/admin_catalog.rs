//! The admin console's words: its sections, and what each setting is called
//! and does. DOM-free and host-tested.
//!
//! The burrow says what a setting *is* (its value, default, shape, choices:
//! [`crate::admin_settings`]). What it is *called* and what it *means to an
//! operator* is copy, and copy is the client's. A key with no entry here is
//! not hidden: it lands in Advanced under its own name, so a newer burrow's
//! settings are reachable from an older client.
//!
//! A host test checks this file against the server's own key list, in both
//! directions: nothing described here that no burrow has, and nothing a burrow
//! has that is neither described here nor deliberately left to Advanced.

/// What a number measures, which decides the echo beside its field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// A plain count, or not a number.
    None,
    /// Bytes: echoed as `1.5 GiB`.
    Bytes,
    /// Bytes per second: echoed as `1.5 MiB per second`.
    BytesPerSec,
    /// Seconds: echoed as `2 hours`.
    Seconds,
}

/// One setting's copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The config key.
    pub key: &'static str,
    /// What the row is called.
    pub label: &'static str,
    /// What changing it does, in a sentence or two.
    pub help: &'static str,
    /// What a number here measures.
    pub unit: Unit,
    /// A multi-line text box.
    pub long: bool,
    /// What `0` means, where it means something other than zero.
    pub zero: &'static str,
    /// `(value, label)` for a dropdown's options. A value the burrow offers
    /// and this list lacks is shown as itself.
    pub choices: &'static [(&'static str, &'static str)],
}

const fn item(key: &'static str, label: &'static str, help: &'static str) -> Item {
    Item {
        key,
        label,
        help,
        unit: Unit::None,
        long: false,
        zero: "",
        choices: &[],
    }
}

impl Item {
    const fn unit(self, unit: Unit) -> Self {
        Item { unit, ..self }
    }
    const fn long(self) -> Self {
        Item { long: true, ..self }
    }
    const fn zero(self, zero: &'static str) -> Self {
        Item { zero, ..self }
    }
    const fn choices(self, choices: &'static [(&'static str, &'static str)]) -> Self {
        Item { choices, ..self }
    }

    /// The label for one of a dropdown's values.
    pub fn choice_label(&self, value: &str) -> String {
        self.choices
            .iter()
            .find(|(v, _)| *v == value)
            .map(|(_, l)| (*l).to_string())
            .unwrap_or_else(|| value.to_string())
    }
}

/// Settings that belong together, under one heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    /// The heading.
    pub title: &'static str,
    /// A line under it, when the group needs one.
    pub blurb: &'static str,
    /// Its settings, in order.
    pub items: &'static [Item],
}

/// What a section's pane holds besides (or instead of) settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// Settings groups only.
    Settings,
    /// Settings, then the feed table and monitor.
    Feeds,
    /// Accounts, classes and invites.
    People,
    /// The board tree.
    Boards,
    /// The file areas.
    Areas,
    /// Broadcast and kick.
    Moderation,
    /// Federation peers and trusted origins.
    Peers,
    /// Snapshots.
    Backups,
    /// The theme editor.
    Appearance,
    /// Every setting this client has no words for.
    Advanced,
}

/// Which half of the navbar a section sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    /// How the burrow is configured.
    Settings,
    /// The people and things in it.
    Manage,
}

impl Area {
    /// The navbar heading.
    pub fn title(self) -> &'static str {
        match self {
            Area::Settings => "Settings",
            Area::Manage => "Manage",
        }
    }
}

/// One entry in the console's navbar, and the pane it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// The route segment: `/admin/<id>`.
    pub id: &'static str,
    /// The navbar label and the pane's heading.
    pub title: &'static str,
    /// What the pane is for.
    pub blurb: &'static str,
    /// Which half of the navbar.
    pub area: Area,
    /// What the pane holds.
    pub pane: Pane,
    /// Its settings groups.
    pub groups: &'static [Group],
}

const MIN_ROLE: &[(&str, &str)] = &[
    ("guest", "Everyone, guests included"),
    ("user", "Members"),
    ("moderator", "Moderators and admins"),
    ("admin", "Admins only"),
];

const ADDR_HELP_RESTART: &str = "The address and port it listens on, as host:port. \
     0.0.0.0 listens on every network interface.";

/// Every section, in navbar order. The first is where `/admin` lands.
pub const SECTIONS: &[Section] = &[
    Section {
        id: "burrow",
        title: "Burrow",
        blurb: "What this place is called, and what it says to people when they arrive.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[
            Group {
                title: "Identity",
                blurb: "",
                items: &[
                    item(
                        "name",
                        "Name",
                        "Shown in everyone\u{2019}s burrow list, in the header, and in the \
                         directory if this burrow is listed.",
                    ),
                    item(
                        "motd",
                        "Message of the day",
                        "Sent to each person as they sign in. Leave it empty to say nothing.",
                    )
                    .long(),
                    item(
                        "agreement",
                        "Agreement",
                        "Rules a person must accept before they can take part. Leave it empty \
                         and nobody is asked.",
                    )
                    .long(),
                ],
            },
            Group {
                title: "Welcome screen",
                blurb: "",
                items: &[
                    item(
                        "welcome_featured",
                        "Featured block",
                        "A notice on the welcome screen. The first line is its title, the rest \
                         is its body.",
                    )
                    .long(),
                    item(
                        "welcome_ticker",
                        "Ticker",
                        "One line that runs across the welcome screen. Also used as the \
                         directory description when that is empty.",
                    ),
                ],
            },
            Group {
                title: "Backups",
                blurb: "",
                items: &[item(
                    "backup_dir",
                    "Snapshot folder",
                    "Where a snapshot made from Backups is written. A relative path is inside \
                     the data folder.",
                )],
            },
        ],
    },
    Section {
        id: "access",
        title: "Access",
        blurb: "Who may come in, how long they stay signed in, and how much room a profile gets.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[
            Group {
                title: "Joining",
                blurb: "",
                items: &[
                    item(
                        "registration_mode",
                        "New accounts",
                        "Who can make an account. With invites, only someone holding an invite \
                         code from People can register.",
                    )
                    .choices(&[
                        ("open", "Anyone can register"),
                        ("invite", "Invite code required"),
                        ("closed", "Nobody can register"),
                    ]),
                    item(
                        "guest_enabled",
                        "Guests",
                        "Let people look around without an account. Guests get the guest \
                         class\u{2019}s permissions and no direct messages.",
                    ),
                ],
            },
            Group {
                title: "Sessions",
                blurb: "",
                items: &[item(
                    "session_ttl_secs",
                    "Stay signed in for",
                    "How long a sign-in lasts before the app has to ask for the password \
                     again, in seconds.",
                )
                .unit(Unit::Seconds)],
            },
            Group {
                title: "Profiles",
                blurb: "",
                items: &[
                    item(
                        "persona_max",
                        "Personas per account",
                        "How many alternate names one account may keep.",
                    ),
                    item(
                        "avatar_max_bytes",
                        "Largest avatar",
                        "The biggest avatar image the burrow will store, in bytes.",
                    )
                    .unit(Unit::Bytes),
                    item(
                        "banner_max_bytes",
                        "Largest banner",
                        "The biggest profile or theme banner the burrow will store, in bytes.",
                    )
                    .unit(Unit::Bytes),
                ],
            },
        ],
    },
    Section {
        id: "limits",
        title: "Chat & limits",
        blurb: "How much one person or one address may do, and how fast.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[
            Group {
                title: "Chat",
                blurb: "",
                items: &[item(
                    "chat_max_len",
                    "Longest chat line",
                    "A longer line is refused. Counted in bytes, so an emoji costs about four.",
                )
                .unit(Unit::Bytes)],
            },
            Group {
                title: "Rate limits",
                blurb: "Each limit is a steady rate plus a burst: the burst is how many can \
                        happen at once before the rate applies.",
                items: &[
                    item(
                        "ratelimit_enabled",
                        "Rate limiting",
                        "The master switch. Off, none of the limits below apply.",
                    ),
                    item(
                        "ratelimit_conn_per_min",
                        "Connections per minute",
                        "New connections allowed from one address, across every way in.",
                    )
                    .zero("not limited"),
                    item(
                        "ratelimit_conn_burst",
                        "Connection burst",
                        "How many connections one address may open at once.",
                    )
                    .zero("every connection is refused"),
                    item(
                        "ratelimit_auth_per_min",
                        "Failed sign-ins per minute",
                        "Wrong passwords allowed from one address, on every surface. A \
                         sign-in that succeeds never counts.",
                    )
                    .zero("not limited"),
                    item(
                        "ratelimit_auth_burst",
                        "Failed sign-in burst",
                        "How many wrong passwords in a row before the rate applies.",
                    )
                    .zero("every attempt is refused"),
                    item(
                        "ratelimit_msg_per_sec",
                        "Messages per second",
                        "Chat lines and direct messages one account may send.",
                    )
                    .zero("not limited"),
                    item(
                        "ratelimit_msg_burst",
                        "Message burst",
                        "How many messages at once before the rate applies.",
                    )
                    .zero("every message is refused"),
                    item(
                        "ratelimit_post_per_min",
                        "Board posts per minute",
                        "Posts one account may make, from the app, a newsreader or Hotline.",
                    )
                    .zero("not limited"),
                    item(
                        "ratelimit_post_burst",
                        "Post burst",
                        "How many posts at once before the rate applies.",
                    )
                    .zero("every post is refused"),
                    item(
                        "ratelimit_transfer_per_min",
                        "Transfers per minute",
                        "File transfers one account may start.",
                    )
                    .zero("not limited"),
                    item(
                        "ratelimit_transfer_burst",
                        "Transfer burst",
                        "How many transfers at once before the rate applies.",
                    )
                    .zero("every transfer is refused"),
                    item(
                        "ratelimit_legacy_per_sec",
                        "Gateway commands per second",
                        "Commands one address may send over telnet, Hotline or NNTP.",
                    )
                    .zero("not limited"),
                    item(
                        "ratelimit_legacy_burst",
                        "Gateway command burst",
                        "How many gateway commands at once before the rate applies.",
                    )
                    .zero("every command is refused"),
                ],
            },
        ],
    },
    Section {
        id: "files",
        title: "Files & transfers",
        blurb: "How much people may upload, how fast files move, and what the swarm may keep.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[
            Group {
                title: "Uploads",
                blurb: "What one person may put in the file library. Checked on every surface \
                        that takes files: the app, Hotline, telnet and the rest.",
                items: &[
                    item(
                        "upload_max_file_bytes",
                        "Largest file",
                        "The biggest single file anyone may upload, in bytes. A larger one is \
                         refused before any of it is sent. Hotline and telnet uploads also stop \
                         at 64 MiB, whatever this says.",
                    )
                    .unit(Unit::Bytes)
                    .zero("no limit"),
                    item(
                        "upload_quota_bytes",
                        "Space per person",
                        "How much one account may keep in the file library altogether, in \
                         bytes. Removing a file gives its space back.",
                    )
                    .unit(Unit::Bytes)
                    .zero("no quota"),
                ],
            },
            Group {
                title: "Transfers",
                blurb: "",
                items: &[
                    item(
                        "max_concurrent_transfers",
                        "Transfers at once",
                        "How many uploads and downloads one account may run together.",
                    )
                    .zero("no limit"),
                    item(
                        "transfer_rate_bytes_per_sec",
                        "Download speed cap",
                        "The fastest a single download may go, in bytes per second.",
                    )
                    .unit(Unit::BytesPerSec)
                    .zero("no cap"),
                ],
            },
            Group {
                title: "Swarm",
                blurb: "People who hold a file can offer it to others here. The burrow keeps \
                        the list of who has what.",
                items: &[
                    item(
                        "swarm_advert_ttl_secs",
                        "Longest offer",
                        "How long an offer to share a file stands before it has to be renewed, \
                         in seconds.",
                    )
                    .unit(Unit::Seconds),
                    item(
                        "swarm_adverts_max",
                        "Offers per account",
                        "How many files one account may offer at a time.",
                    )
                    .zero("no limit"),
                    item(
                        "swarm_cache_max_bytes",
                        "Cache size",
                        "Disk the burrow may use for files nothing refers to any more, in \
                         bytes. Over this, the oldest go first.",
                    )
                    .unit(Unit::Bytes)
                    .zero("keep everything"),
                ],
            },
            Group {
                title: "Download links",
                blurb: "",
                items: &[item(
                    "files_http_base",
                    "Link address",
                    "The public address of the web surface, for the download links telnet \
                     hands out (https://bbs.example.org:8080). Empty: telnet offers no links.",
                )],
            },
        ],
    },
    Section {
        id: "discovery",
        title: "Discovery",
        blurb: "Whether people who do not already know this burrow\u{2019}s address can find it.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[Group {
            title: "Directory listing",
            blurb: "A burrow is listed only once it can say where it is: with no public \
                    hostname below, listing stays off whatever this switch says.",
            items: &[
                item(
                    "announce_enabled",
                    "List this burrow",
                    "Announce to the directories below. Off is a real opt-out: the burrow \
                     also asks anyone who finds it not to pass it on.",
                ),
                item(
                    "advertise_host",
                    "Public hostname",
                    "The name people reach this burrow by (rabbithole.example). Empty: \
                     nothing is announced.",
                ),
                item(
                    "ws_public_url",
                    "Public web socket address",
                    "The wss:// address browsers should use, normally a TLS proxy in front \
                     of the burrow. Empty: no browser address is published.",
                ),
                item(
                    "announce_description",
                    "Description",
                    "One line for the directory, up to 240 characters. Empty: the welcome \
                     ticker is used.",
                ),
                item(
                    "announce_sysop",
                    "Operator handle",
                    "The name before the @ in the listing (alice in alice@wonderland). \
                     Empty: taken from the burrow\u{2019}s name.",
                ),
                item(
                    "announce_slug",
                    "Directory name",
                    "The short name to claim in the directory. Empty: the directory picks \
                     one from the burrow\u{2019}s name.",
                ),
                item(
                    "announce_ttl_secs",
                    "Announce every",
                    "Seconds between announcements, from 30 to 3600. A directory marks the \
                     burrow offline after missing two.",
                )
                .unit(Unit::Seconds),
                item(
                    "announce_trackers",
                    "Directories",
                    "Where this burrow announces itself. Edited in burrow.toml.",
                ),
            ],
        }],
    },
    Section {
        id: "network",
        title: "Network",
        blurb: "The addresses this burrow listens on. A wrong one here can lock everyone out, \
                you included.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[
            Group {
                title: "App connections",
                blurb: "",
                items: &[
                    item(
                        "quic_addr",
                        "Main listener",
                        "Where the desktop app and other native clients connect (QUIC), as \
                         host:port.",
                    ),
                    item(
                        "ws_addr",
                        "Web socket listener",
                        "Where browsers and the web client connect, as host:port. Keep it on \
                         127.0.0.1 and put a TLS proxy in front of it.",
                    ),
                    item(
                        "ws_allow_insecure_remote",
                        "Allow unencrypted remote web sockets",
                        "Lets the web socket listen beyond this machine with no TLS. \
                         Passwords and sign-in tokens then cross the network in the clear. \
                         Leave this off.",
                    ),
                    item(
                        "ws_allowed_origins",
                        "Allowed web origins",
                        "The sites allowed to open a web socket to this burrow. Edited in \
                         burrow.toml.",
                    ),
                ],
            },
            Group {
                title: "Web surface",
                blurb: "",
                items: &[
                    item(
                        "http_enabled",
                        "Serve the web client",
                        "Answer on the address below with the web client and with file \
                         download links.",
                    ),
                    item("http_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "http_web_root",
                        "Web client folder",
                        "The folder holding the built web client. Empty: only download links \
                         are served. A relative path is inside the data folder.",
                    ),
                ],
            },
            Group {
                title: "Port mapping",
                blurb: "",
                items: &[
                    item(
                        "portmap_enabled",
                        "Ask the router to open ports",
                        "At startup, ask a home router to forward the app ports to this \
                         machine (NAT-PMP or PCP). Does nothing until a router is named below.",
                    ),
                    item(
                        "portmap_gateway",
                        "Router address",
                        "The router\u{2019}s address on your network (192.168.1.1). It is not \
                         discovered for you.",
                    ),
                    item(
                        "portmap_lifetime_secs",
                        "Mapping lifetime",
                        "How long each mapping is asked for, in seconds. It is renewed at \
                         about half this.",
                    )
                    .unit(Unit::Seconds),
                ],
            },
            Group {
                title: "Storage",
                blurb: "",
                items: &[item(
                    "data_dir",
                    "Data folder",
                    "Where the database, files, keys and control socket live. Set when the \
                     burrow is started.",
                )],
            },
        ],
    },
    Section {
        id: "radio",
        title: "Radio",
        blurb: "The burrow\u{2019}s own streaming server: what listeners tune in to, and how a \
                DJ goes live.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[
            Group {
                title: "Listening",
                blurb: "",
                items: &[
                    item(
                        "radio_enabled",
                        "Radio",
                        "Stream the burrow\u{2019}s stations. The app is told where to tune \
                         in; nobody types an address.",
                    ),
                    item("radio_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "radio_public_base",
                        "Public stream address",
                        "Set this when listeners reach the stream somewhere else, such as \
                         behind a TLS proxy (https://radio.example.org). Empty: the host they \
                         connected to, on the port above.",
                    ),
                ],
            },
            Group {
                title: "DJ sources",
                blurb: "A DJ streams in with Icecast-compatible software. While they are live \
                        they replace the station\u{2019}s rotation.",
                items: &[
                    item(
                        "radio_source_enabled",
                        "Accept live DJs",
                        "Listen for source connections on the address below.",
                    ),
                    item("radio_source_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "radio_source_user",
                        "Source username",
                        "The username a DJ\u{2019}s software must give.",
                    ),
                    item(
                        "radio_source_password",
                        "Source password",
                        "The password a DJ\u{2019}s software must give. With none set, every \
                         source is refused.",
                    ),
                ],
            },
        ],
    },
    Section {
        id: "gateways",
        title: "Gateways",
        blurb: "Older protocols, so older clients can reach the same boards, files and chat. \
                Each is off until you turn it on.",
        area: Area::Settings,
        pane: Pane::Settings,
        groups: &[
            Group {
                title: "Telnet",
                blurb: "The burrow as a text BBS.",
                items: &[
                    item(
                        "telnet_enabled",
                        "Telnet",
                        "Serve the text-mode BBS on the address below.",
                    ),
                    item("telnet_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "telnet_min_role",
                        "Who may use it",
                        "Anyone below this is refused at sign-in.",
                    )
                    .choices(MIN_ROLE),
                ],
            },
            Group {
                title: "Door games",
                blurb: "Classic door games, run from the telnet BBS. The games themselves are \
                        installed in burrow.toml.",
                items: &[
                    item(
                        "doors_enabled",
                        "Door games",
                        "Let telnet callers launch the installed doors. Needs telnet.",
                    ),
                    item(
                        "doors_dir",
                        "Working folder",
                        "Where each door session keeps its drop files. A relative path is \
                         inside the data folder.",
                    ),
                    item(
                        "doors_max_nodes",
                        "Doors at once",
                        "How many door sessions may run together.",
                    )
                    .zero("no door can start"),
                    item(
                        "doors_session_max_secs",
                        "Longest session",
                        "How long one door session may last, in seconds. A door\u{2019}s own \
                         daily limit can shorten it.",
                    )
                    .unit(Unit::Seconds)
                    .zero("no limit"),
                ],
            },
            Group {
                title: "Offline mail",
                blurb: "QWK packets, for reading the boards offline.",
                items: &[
                    item(
                        "qwk_enabled",
                        "QWK packets",
                        "Offer the qwk command on telnet.",
                    ),
                    item(
                        "qwk_spool_dir",
                        "Spool folder",
                        "Where packets are built, one folder per person. A relative path is \
                         inside the data folder.",
                    ),
                ],
            },
            Group {
                title: "Finger",
                blurb: "Answers \u{201c}who is on?\u{201d} for finger clients. Finger has no \
                        sign-in, so everyone counts as a guest.",
                items: &[
                    item(
                        "finger_enabled",
                        "Finger",
                        "Answer finger queries on the address below.",
                    ),
                    item("finger_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "finger_min_role",
                        "Who may ask",
                        "Anything above guests refuses every query, politely.",
                    )
                    .choices(MIN_ROLE),
                ],
            },
            Group {
                title: "Newsreaders",
                blurb: "The boards as newsgroups (NNTP), for reading and posting from a \
                        newsreader.",
                items: &[
                    item(
                        "nntp_enabled",
                        "NNTP",
                        "Serve newsreaders on the address below.",
                    ),
                    item("nntp_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "nntp_min_role",
                        "Who may read",
                        "Reading without signing in counts as a guest. Above that, a \
                         newsreader must sign in first.",
                    )
                    .choices(MIN_ROLE),
                    item(
                        "nntp_tls_enabled",
                        "NNTP over TLS",
                        "Also serve newsreaders over TLS, with the burrow\u{2019}s own \
                         certificate. Works with or without the plain listener.",
                    ),
                    item("nntp_tls_addr", "TLS address", ADDR_HELP_RESTART),
                    item(
                        "nntp_auth_require_tls",
                        "Passwords need TLS",
                        "Refuse a sign-in on an unencrypted connection, so passwords never \
                         cross the network in the clear. Applies to news peers too.",
                    ),
                ],
            },
            Group {
                title: "News peers",
                blurb: "Exchange posts with other news servers. Peers and their passwords are \
                        listed in burrow.toml; with none listed, every peer is refused.",
                items: &[
                    item(
                        "nntp_feed_enabled",
                        "Peer feed",
                        "Accept posts from news peers on the address below.",
                    ),
                    item("nntp_feed_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "nntp_feed_tls_enabled",
                        "Peer feed over TLS",
                        "Also accept peers over TLS, with the burrow\u{2019}s own certificate.",
                    ),
                    item("nntp_feed_tls_addr", "TLS address", ADDR_HELP_RESTART),
                ],
            },
            Group {
                title: "Hotline",
                blurb: "For Hotline clients: chat, news and files.",
                items: &[
                    item(
                        "hotline_enabled",
                        "Hotline",
                        "Serve Hotline clients on the address below, and on the next port up \
                         for transfers.",
                    ),
                    item("hotline_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "hotline_min_role",
                        "Who may use it",
                        "A Hotline guest sign-in counts as a guest. Anyone below this is \
                         refused.",
                    )
                    .choices(MIN_ROLE),
                ],
            },
            Group {
                title: "FidoNet",
                blurb: "Exchange echomail with a FidoNet uplink (binkp). Which echo feeds \
                        which board is mapped in burrow.toml.",
                items: &[
                    item(
                        "ftn_enabled",
                        "FidoNet mailer",
                        "Accept binkp sessions on the address below.",
                    ),
                    item("ftn_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "ftn_node",
                        "This node",
                        "This system\u{2019}s FidoNet address (2:280/464). Empty: mail is \
                         accepted and never tossed.",
                    ),
                    item(
                        "ftn_uplink",
                        "Uplink node",
                        "The FidoNet address outbound mail is sent to (2:280/1).",
                    ),
                    item(
                        "ftn_uplink_host",
                        "Uplink host",
                        "Where to dial the uplink, as host:port (hub.example.org:24554).",
                    ),
                    item(
                        "ftn_password",
                        "Session password",
                        "The binkp password shared with the uplink. With none set, sessions \
                         are unsecured.",
                    ),
                    item(
                        "ftn_inbound_dir",
                        "Inbound folder",
                        "Where received packets land. A relative path is inside the data \
                         folder.",
                    ),
                    item(
                        "ftn_outbound_dir",
                        "Outbound folder",
                        "Where packets wait to be sent. A relative path is inside the data \
                         folder.",
                    ),
                ],
            },
        ],
    },
    Section {
        id: "federation",
        title: "Federation & feeds",
        blurb: "What this burrow exchanges with other burrows, and what it pulls in from the web.",
        area: Area::Settings,
        pane: Pane::Feeds,
        groups: &[
            Group {
                title: "Federation",
                blurb: "Approved burrows share catalogs, search and board posts. Peers are \
                        approved under Peers.",
                items: &[
                    item(
                        "federation_enabled",
                        "Federation",
                        "Listen for other burrows on the address below, and dial the peers \
                         listed in burrow.toml.",
                    ),
                    item("federation_addr", "Address", ADDR_HELP_RESTART),
                    item(
                        "federation_origin",
                        "This burrow\u{2019}s origin",
                        "The permanent name this burrow signs federated posts with. Set once \
                         in burrow.toml and never changed.",
                    ),
                ],
            },
            Group {
                title: "Sending files between burrows",
                blurb: "Someone on this burrow and another can send files and folders from one \
                        to the other. The receiving burrow fetches them itself: from an \
                        approved peer over their federation session, and from any other burrow \
                        only when both operators allow it below.",
                items: &[
                    item(
                        "s2s_grants_enabled",
                        "Let people send from here",
                        "People may send what they can download here to another burrow: a \
                         peer, or any burrow if allowed below. The permission names that burrow \
                         alone and lapses after an hour.",
                    ),
                    item(
                        "s2s_pull_enabled",
                        "Take files sent from other burrows",
                        "People may bring files from another burrow into this one: a peer, or \
                         any burrow if allowed below. They are filed under the person who \
                         asked, and count against their space.",
                    ),
                    item(
                        "s2s_max_concurrent",
                        "Sends at once, per person",
                        "How many sends one person may have coming in at the same time.",
                    )
                    .zero("no limit"),
                    item(
                        "s2s_max_bytes",
                        "Largest send",
                        "The most one send may bring in altogether, in bytes. The largest file \
                         and each person\u{2019}s space still apply.",
                    )
                    .unit(Unit::Bytes)
                    .zero("no limit beyond those"),
                    item(
                        "s2s_grants_to_any",
                        "Send to burrows that are not peers",
                        "Let people send to any burrow they are also on, not only approved \
                         peers. That burrow connects to this one\u{2019}s QUIC port and proves \
                         it is the one named; people still send only what they may download.",
                    ),
                    item(
                        "s2s_pull_from_any",
                        "Take sends from burrows that are not peers",
                        "Let people bring files from any burrow they are also on. This burrow \
                         connects to that one to fetch them, and files them under the person \
                         as any send.",
                    ),
                    item(
                        "s2s_private_addresses",
                        "Reach private addresses",
                        "Let those connections go to private and local addresses: for burrows \
                         on one network or one machine. Off, only public addresses are dialed, \
                         so a send cannot be used to reach this burrow\u{2019}s own network.",
                    ),
                    item(
                        "s2s_swarm_sources",
                        "Offer this burrow\u{2019}s swarm to receiving burrows",
                        "A burrow a file is sent to may also fetch it from people here who \
                         seed it, which is faster. Their addresses reach that burrow; \
                         invisible people are never offered.",
                    ),
                    item(
                        "s2s_swarm",
                        "Fetch sends from the sender\u{2019}s swarm",
                        "Fetch larger files sent here from the sending burrow\u{2019}s seeders \
                         too, when it offers them, and from the sender for the rest. Each of \
                         those seeders sees this burrow\u{2019}s address.",
                    ),
                ],
            },
            Group {
                title: "Web feeds",
                blurb: "RSS and Atom feeds posted into boards. Which feed goes to which board \
                        is mapped in burrow.toml.",
                items: &[
                    item(
                        "syndication_enabled",
                        "Fetch feeds",
                        "Check the mapped feeds and post what is new.",
                    ),
                    item(
                        "syndication_poll_secs",
                        "Check every",
                        "Seconds between checks. The burrow never checks a feed more often \
                         than every 5 minutes, and backs off a feed that fails.",
                    )
                    .unit(Unit::Seconds),
                ],
            },
        ],
    },
    Section {
        id: "people",
        title: "People",
        blurb: "Accounts, the classes that decide what they may do, and invitations.",
        area: Area::Manage,
        pane: Pane::People,
        groups: &[],
    },
    Section {
        id: "boards",
        title: "Boards",
        blurb: "The message boards, and the categories that group them.",
        area: Area::Manage,
        pane: Pane::Boards,
        groups: &[],
    },
    Section {
        id: "areas",
        title: "File areas",
        blurb: "The libraries people browse, upload to and download from.",
        area: Area::Manage,
        pane: Pane::Areas,
        groups: &[],
    },
    Section {
        id: "moderation",
        title: "Moderation",
        blurb: "What people reported, who is connected, what is refused, and what operators \
                did.",
        area: Area::Manage,
        pane: Pane::Moderation,
        groups: &[],
    },
    Section {
        id: "appearance",
        title: "Appearance",
        blurb: "The theme this burrow offers the people in it.",
        area: Area::Manage,
        pane: Pane::Appearance,
        groups: &[],
    },
    Section {
        id: "peers",
        title: "Peers",
        blurb: "The burrows this one talks to: who has asked to peer, who is approved, and \
                whose keys are trusted.",
        area: Area::Manage,
        pane: Pane::Peers,
        groups: &[],
    },
    Section {
        id: "backups",
        title: "Backups",
        blurb: "Snapshots of everything here, made and checked from where you sit.",
        area: Area::Manage,
        pane: Pane::Backups,
        groups: &[],
    },
    Section {
        id: "advanced",
        title: "Advanced",
        blurb: "Settings this version of the app has no description for, by the names the \
                burrow gives them.",
        area: Area::Manage,
        pane: Pane::Advanced,
        groups: &[],
    },
];

/// Keys that have a home of their own and are not settings rows: the theme
/// editor writes every `theme_*` key as one published bundle.
pub fn owned_elsewhere(key: &str) -> bool {
    key.starts_with("theme_")
}

/// The section for a route segment; the first section when there is none or
/// it names nothing.
pub fn section(id: Option<&str>) -> &'static Section {
    id.and_then(|id| SECTIONS.iter().find(|s| s.id == id))
        .unwrap_or(&SECTIONS[0])
}

/// Every key some section describes.
pub fn described_keys() -> impl Iterator<Item = &'static str> {
    SECTIONS
        .iter()
        .flat_map(|s| s.groups.iter())
        .flat_map(|g| g.items.iter())
        .map(|i| i.key)
}

/// Whether some section describes `key`. Anything else the burrow reports
/// (and that has no home of its own) belongs in Advanced.
pub fn is_described(key: &str) -> bool {
    described_keys().any(|k| k == key)
}

/// The Advanced row for a key nobody described: the key is its own label.
pub fn undescribed(key: &'static str) -> Item {
    item(key, key, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_server_core::config::CONFIG_KEYS;
    use std::collections::HashSet;

    #[test]
    fn every_setting_a_burrow_has_is_described_or_deliberately_left_out() {
        let described: HashSet<&str> = described_keys().collect();
        let missing: Vec<&&str> = CONFIG_KEYS
            .iter()
            .filter(|k| !described.contains(**k) && !owned_elsewhere(k))
            .collect();
        assert!(
            missing.is_empty(),
            "settings with no words (they would sit in Advanced): {missing:?}"
        );
    }

    #[test]
    fn nothing_is_described_that_no_burrow_has_and_nothing_twice() {
        let real: HashSet<&str> = CONFIG_KEYS.iter().copied().collect();
        let mut seen = HashSet::new();
        for key in described_keys() {
            assert!(real.contains(key), "{key} is described and does not exist");
            assert!(seen.insert(key), "{key} is described twice");
            assert!(!owned_elsewhere(key), "{key} belongs to the theme editor");
        }
    }

    #[test]
    fn dropdown_labels_cover_exactly_what_a_burrow_offers() {
        let info = rabbithole_server_core::ServerConfig::default().describe();
        for i in info.iter().filter(|i| !i.choices.is_empty()) {
            let copy = SECTIONS
                .iter()
                .flat_map(|s| s.groups.iter())
                .flat_map(|g| g.items.iter())
                .find(|it| it.key == i.key)
                .unwrap_or_else(|| panic!("{} has choices and no copy", i.key));
            let labelled: Vec<&str> = copy.choices.iter().map(|(v, _)| *v).collect();
            assert_eq!(labelled, i.choices, "{}", i.key);
        }
    }

    #[test]
    fn units_are_only_claimed_for_numbers() {
        let info = rabbithole_server_core::ServerConfig::default().describe();
        for it in SECTIONS
            .iter()
            .flat_map(|s| s.groups.iter())
            .flat_map(|g| g.items.iter())
        {
            let kind = info.iter().find(|i| i.key == it.key).unwrap().kind;
            let number = kind == rabbithole_server_core::config::KeyKind::Number;
            assert!(
                it.unit == Unit::None || number,
                "{} has a unit and is not a number",
                it.key
            );
            assert!(
                it.zero.is_empty() || number,
                "{} explains 0 and is not a number",
                it.key
            );
            assert!(
                !it.long || kind == rabbithole_server_core::config::KeyKind::Text,
                "{}",
                it.key
            );
        }
    }

    #[test]
    fn copy_is_written_for_people() {
        let mut ids = HashSet::new();
        for s in SECTIONS {
            assert!(ids.insert(s.id), "{} twice", s.id);
            assert!(s.blurb.ends_with('.'), "{}", s.id);
            assert!(
                s.id.chars().all(|c| c.is_ascii_lowercase()),
                "{} is a route segment",
                s.id
            );
            for g in s.groups {
                assert!(!g.items.is_empty(), "{} / {} is empty", s.id, g.title);
                for it in g.items {
                    assert!(!it.label.contains('_'), "{} is labelled with a key", it.key);
                    assert!(it.help.ends_with('.'), "{} help is not a sentence", it.key);
                    assert!(
                        !it.help.contains('\u{2014}'),
                        "{} help has an em dash",
                        it.key
                    );
                }
            }
        }
        assert_eq!(section(None).id, "burrow");
        assert_eq!(section(Some("gateways")).id, "gateways");
        assert_eq!(section(Some("nonsense")).id, "burrow");
    }
}
