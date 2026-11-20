//! `ridgewarden-status.toml`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ridgewarden_client::alerts::AlertCfg;
use ridgewarden_client::util::{norm_node, valid_node};
use ridgewarden_linkdoctor::config::LinksCfg;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub core: CoreCfg,
    #[serde(default = "d_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "d_listen")]
    pub listen: SocketAddr,
    /// anything older than this shows as UNKNOWN, never green
    #[serde(default = "d_fresh")]
    pub fresh_s: u64,
    /// failed checks in a row before a service counts as down, and good ones before it is
    /// back up; keep both the same as ridgewarden-health's [probe] settings
    #[serde(default = "d_fail_after")]
    pub fail_after: u32,
    #[serde(default = "d_ok_after")]
    pub ok_after: u32,
    #[serde(default = "d_title")]
    pub title: String,
    /// services shown on the page (case-insensitive substring); empty shows all
    #[serde(default)]
    pub key_services: Vec<String>,
    /// public keys (hex) allowed to post the banner, incidents and maintenance notices;
    /// empty allows any station in the log
    #[serde(default)]
    pub banner_authors: Vec<String>,
    /// friendly site names: node name -> label, e.g. "tst-hilltop" = "Hilltop relay"
    #[serde(default)]
    pub site_names: BTreeMap<String, String>,
    /// AREDN message groups: group name -> node names, served as /aredn/<group>.txt
    #[serde(default)]
    pub aredn_groups: BTreeMap<String, Vec<String>>,
    /// sites that are always listed, even before they are first heard from
    #[serde(default)]
    pub expected_sites: Vec<String>,
    /// who runs this page and how to reach them, shown at its foot
    #[serde(default)]
    pub contact: String,
    #[serde(default)]
    pub links: LinksCfg,
    /// Configure this on one site only, or every site sends the same alert.
    #[serde(default)]
    pub alerts: AlertCfg,
    #[serde(default)]
    pub mirror: MirrorCfg,
    /// Like alerts, configure this on one site only.
    #[serde(default)]
    pub subscriptions: SubscriptionsCfg,
}

/// A read-only copy of the pages, written as static files and pushed to a web server off
/// the mesh. Set `dir`, `command` or `put_url` to turn it on.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct MirrorCfg {
    /// write the files here; without it they go to a folder in data_dir
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// run after the files are written, with {dir} replaced by their folder
    #[serde(default)]
    pub command: Option<String>,
    /// PUT each file to this URL plus the file's name
    #[serde(default)]
    pub put_url: Option<String>,
    /// a file holding a bearer token for the PUTs
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    #[serde(default = "d_mirror_every")]
    pub every_s: u64,
    /// services the public copy may name (case-insensitive substring); it names none
    /// otherwise, since a service list says a lot about a network
    #[serde(default)]
    pub services: Vec<String>,
}

impl MirrorCfg {
    pub fn enabled(&self) -> bool {
        self.dir.is_some() || self.command.is_some() || self.put_url.is_some()
    }

    fn validate(&self) -> Result<()> {
        if self.every_s < 30 {
            bail!("mirror.every_s must be at least 30");
        }
        if let Some(u) = &self.put_url
            && !web_url(u)
        {
            bail!("mirror.put_url {u:?} is not an http(s) URL");
        }
        if self.token_file.is_some() && self.put_url.is_none() {
            bail!("mirror.token_file is only used with put_url");
        }
        if self.command.as_deref().is_some_and(|c| c.trim().is_empty()) {
            bail!("mirror.command is empty");
        }
        Ok(())
    }
}

fn web_url(u: &str) -> bool {
    u.starts_with("http://") || u.starts_with("https://")
}

impl SubscriptionsCfg {
    fn validate(&self) -> Result<()> {
        let mut names = std::collections::BTreeSet::new();
        for c in &self.contacts {
            if c.name.trim().is_empty() || !names.insert(c.name.trim().to_ascii_lowercase()) {
                bail!(
                    "every [[subscriptions.contact]] needs a name of its own; {:?} is empty or used twice",
                    c.name
                );
            }
            if c.topics.is_empty() || c.topics.iter().any(|t| !TOPICS.contains(&t.as_str())) {
                bail!(
                    "contact {:?}: topics are any of \"incidents\", \"maintenance\" and \"sites\"",
                    c.name
                );
            }
            if c.email.is_empty() && c.sms.is_empty() && c.webhook.is_none() {
                bail!(
                    "contact {:?} has no email, sms or webhook to send to",
                    c.name
                );
            }
            if let Some(s) = c.sites.iter().find(|s| !valid_node(&norm_node(s))) {
                bail!("contact {:?}: {s:?} is not a node name", c.name);
            }
            if (!c.email.is_empty() || !c.sms.is_empty()) && self.from.trim().is_empty() {
                bail!("[subscriptions] needs `from` to send mail to {:?}", c.name);
            }
        }
        for t in crate::subscribe::targets(self) {
            t.send
                .validate()
                .with_context(|| format!("contact {:?}", t.contact.name))?;
        }
        if let Some(l) = &self.link
            && !web_url(l)
        {
            bail!("subscriptions.link {l:?} is not an http(s) URL");
        }
        if self.max_per_hour == 0 {
            bail!("subscriptions.max_per_hour must be at least 1");
        }
        Ok(())
    }
}

impl Default for MirrorCfg {
    fn default() -> Self {
        Self {
            dir: None,
            command: None,
            put_url: None,
            token_file: None,
            every_s: d_mirror_every(),
            services: vec![],
        }
    }
}

/// Agency contacts who get a message when something they care about happens.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionsCfg {
    /// sender address for mail, which goes out through the box's own sendmail
    #[serde(default)]
    pub from: String,
    #[serde(default = "d_sendmail")]
    pub sendmail: String,
    /// a page to point people at in every message, e.g. the internet mirror
    #[serde(default)]
    pub link: Option<String>,
    /// messages per channel per hour; news beyond that waits and goes out together
    #[serde(default = "d_per_hour")]
    pub max_per_hour: u32,
    #[serde(default, rename = "contact")]
    pub contacts: Vec<Contact>,
}

impl Default for SubscriptionsCfg {
    fn default() -> Self {
        Self {
            from: String::new(),
            sendmail: d_sendmail(),
            link: None,
            max_per_hour: d_per_hour(),
            contacts: vec![],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    pub name: String,
    #[serde(default)]
    pub email: Vec<String>,
    /// email-to-SMS gateway addresses, which get one short line
    #[serde(default)]
    pub sms: Vec<String>,
    /// gets each message as a JSON POST, e.g. a local SMS gateway
    #[serde(default)]
    pub webhook: Option<String>,
    /// any of "incidents", "maintenance" and "sites"
    pub topics: Vec<String>,
    /// only news that touches these sites; empty means every site
    #[serde(default)]
    pub sites: Vec<String>,
}

pub const TOPICS: [&str; 3] = ["incidents", "maintenance", "sites"];

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct CoreCfg {
    #[serde(default = "d_core_url")]
    pub url: String,
    /// the core's `local.token`; only needed to post the banner, incidents and maintenance
    #[serde(default = "d_token")]
    pub token_file: PathBuf,
}

fn d_core_url() -> String {
    "http://127.0.0.1:8790".into()
}
fn d_token() -> PathBuf {
    PathBuf::from("/var/lib/ridgewarden/local.token")
}
fn d_data_dir() -> PathBuf {
    PathBuf::from("/var/lib/ridgewarden-status")
}
fn d_listen() -> SocketAddr {
    "0.0.0.0:8793".parse().unwrap()
}
fn d_fresh() -> u64 {
    960
}
fn d_fail_after() -> u32 {
    3
}
fn d_ok_after() -> u32 {
    2
}
fn d_title() -> String {
    "Mesh status".into()
}
fn d_mirror_every() -> u64 {
    60
}
fn d_sendmail() -> String {
    "/usr/sbin/sendmail".into()
}
fn d_per_hour() -> u32 {
    6
}

impl Default for CoreCfg {
    fn default() -> Self {
        Self {
            url: d_core_url(),
            token_file: d_token(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.fresh_s < 60 {
            bail!("fresh_s must be at least 60");
        }
        if self.fail_after == 0 || self.ok_after == 0 {
            bail!("fail_after and ok_after must be at least 1");
        }
        if let Some(s) = self
            .expected_sites
            .iter()
            .find(|s| !valid_node(&norm_node(s)))
        {
            bail!("expected_sites: {s:?} is not a node name");
        }
        for (group, nodes) in &self.aredn_groups {
            if group.eq_ignore_ascii_case("all") {
                bail!("aredn_groups can't have a group called \"all\"; all.txt goes to every node");
            }
            if !valid_node(&group.to_ascii_lowercase()) {
                bail!("aredn_groups name {group:?} can only have letters, digits and hyphens");
            }
            if let Some(n) = nodes.iter().find(|n| !valid_node(&norm_node(n))) {
                bail!("aredn_groups.{group}: {n:?} is not a node name");
            }
        }
        self.mirror.validate()?;
        self.subscriptions.validate()?;
        self.alerts.validate()
    }

    /// Where the mirror's files are written before they are pushed.
    pub fn mirror_dir(&self) -> PathBuf {
        self.mirror
            .dir
            .clone()
            .unwrap_or_else(|| self.data_dir.join("mirror"))
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("status.db")
    }

    /// Whether the station with this key may post the banner, incidents and maintenance.
    pub fn may_post(&self, key: &str) -> bool {
        self.banner_authors.is_empty()
            || self
                .banner_authors
                .iter()
                .any(|k| k.eq_ignore_ascii_case(key))
    }
}

pub const EXAMPLE: &str = r#"# ridgewarden-status configuration
data_dir = "/var/lib/ridgewarden-status"
listen = "0.0.0.0:8793"
fresh_s = 960
# keep these the same as fail_after and ok_after in ridgewarden-health's [probe] section
fail_after = 3
ok_after = 2
title = "Mesh status"
key_services = []          # e.g. ["winlink", "chat"]; empty shows every service
banner_authors = []        # public keys allowed to post notices and incidents; empty = any station
# who runs this page and how to reach them, shown at its foot
contact = ""
# sites to show even before they are first heard from; any site heard from stays listed
# (as cut off once silent) until `ridgewarden-status forget <node>`
expected_sites = []

[core]
url = "http://127.0.0.1:8790"
token_file = "/var/lib/ridgewarden/local.token"

[site_names]
# "tst-hilltop" = "Hilltop relay"

# AREDN nodes in a message group (aredn.@alerts[0].groups) also fetch /aredn/<group>.txt
[aredn_groups]
# coast = ["tst-hilltop", "tst-valley"]

# One alert per open incident. Set these up on one site only, or every site sends the same alert.
[alerts]
# ntfy = "http://10.1.2.3/mesh-alerts"
# discord = "https://discord.com/api/webhooks/..."
# webhooks = []
# public_url = "http://tst-hilltop-pi.local.mesh:8793"
# quiet_hours = "22-6"
# repeat_hours = 4
# [alerts.email]
# to = ["mesh-team@example.org"]
# from = "mesh-status@example.org"

# A read-only copy of the pages on a web server off the mesh, for people who can't reach it.
# Set dir, command or put_url to turn it on; deploy/mirror has an example web server.
[mirror]
# dir = "/var/www/mesh-status"
# command = "rsync -a --delete {dir}/ mirror@web.example.org:/srv/mesh-status/"
# put_url = "https://web.example.org/dav/mesh-status/"
# token_file = "/etc/ridgewarden/mirror.token"
every_s = 60
# services the public copy may name (case-insensitive part of the name); none otherwise
services = []

# Agency contacts who get a message about incidents, maintenance and sites cut off. Mail goes
# out through this box's own sendmail (msmtp, Postfix). Set this up on one site only.
[subscriptions]
# from = "mesh-status@example.org"
# link = "https://status.example.org/"     # added to every message
max_per_hour = 6                           # per channel; more news waits and goes together
# [[subscriptions.contact]]
# name = "County EOC duty desk"
# email = ["eoc-duty@example.org"]
# sms = ["5550100@sms.example.net"]        # an email-to-SMS gateway address: one short line
# webhook = "http://10.1.2.3:8080/sms"     # gets each message as JSON
# topics = ["incidents", "maintenance", "sites"]
# sites = []                               # only these sites; empty means every site
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses() {
        let c: Config = toml::from_str(EXAMPLE).unwrap();
        c.validate().unwrap();
        assert_eq!(c.listen.port(), 8793);
        assert_eq!((c.fail_after, c.ok_after), (3, 2));
    }

    #[test]
    fn group_names_must_be_fetchable() {
        let mut c: Config = toml::from_str(EXAMPLE).unwrap();
        c.aredn_groups
            .insert("Coast".into(), vec!["TST-Hilltop.local.mesh".into()]);
        c.validate().unwrap();
        c.aredn_groups.insert("all".into(), vec![]);
        assert!(c.validate().is_err(), "all.txt is the whole mesh");
        c.aredn_groups.remove("all");
        c.aredn_groups.insert("north county".into(), vec![]);
        assert!(c.validate().is_err(), "a node can't ask for this name");
    }

    #[test]
    fn contacts_are_checked() {
        let mut c: Config = toml::from_str(EXAMPLE).unwrap();
        let eoc: Contact = toml::from_str(
            "name = \"EOC\"\nemail = [\"eoc@example.org\"]\ntopics = [\"incidents\", \"sites\"]",
        )
        .unwrap();
        c.subscriptions.contacts = vec![eoc.clone()];
        assert!(c.validate().is_err(), "mail needs a from address");
        c.subscriptions.from = "status@example.org".into();
        c.validate().unwrap();

        let mut bad = c.clone();
        bad.subscriptions.contacts[0].topics = vec!["weather".into()];
        assert!(bad.validate().is_err());
        let mut bad = c.clone();
        bad.subscriptions.contacts[0].email = vec!["eoc@example.org\nBcc: x@example.org".into()];
        assert!(bad.validate().is_err(), "no header tricks in an address");
        let mut bad = c.clone();
        bad.subscriptions.contacts.push(eoc);
        assert!(
            bad.validate().is_err(),
            "names are how channels are told apart"
        );
    }
}
