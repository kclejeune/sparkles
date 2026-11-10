//! The `notifications` kind that `/$/server/settings/notifications` holds (spec C21 §5),
//! member by member. The web UI builds the form of the Notifications section from these
//! schemas, as it builds the Models section from those of [`super::model_settings`], and
//! uses the same extensions. A channel's members carry `x-sparkles-kinds` with the channel
//! types they apply to, so that a form shows each channel only the members of its type.

use super::kit::*;
use super::*;

fn titled(mut v: J, title: &str, description: &str) -> J {
    v["title"] = title.into();
    v["description"] = description.into();
    v
}

fn types(mut v: J, types: &[&str]) -> J {
    v["x-sparkles-kinds"] = json!(types);
    v
}

fn unset(mut v: J, words: &str) -> J {
    v["x-sparkles-unset"] = words.into();
    v
}

fn default(mut v: J, d: J) -> J {
    v["default"] = d;
    v
}

fn bounded(mut v: J, min: f64, max: f64) -> J {
    v["minimum"] = json!(min);
    v["maximum"] = json!(max);
    v
}

/// A number of seconds above 0 and at most `max`.
fn seconds(title: &str, description: &str, max: f64) -> J {
    let mut v = titled(num(), title, description);
    v["exclusiveMinimum"] = json!(0);
    v["maximum"] = json!(max);
    v["x-sparkles-unit"] = "seconds".into();
    v
}

/// An object of these members in this order, of which `required` must be present.
fn ordered(required: &[&str], order: &[&str], props: J) -> J {
    let mut o = closed(required, props);
    o["x-sparkles-order"] = json!(order);
    o
}

/// A secret reference, `{"secret": NAME}`, whose name the form edits as a secret field.
fn secret_ref(title: &str, description: &str, name_help: &str) -> J {
    let mut name = titled(string(), title, name_help);
    name["pattern"] = "^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$".into();
    name["x-sparkles-secret"] = true.into();
    let mut v = closed(&["secret"], json!({ "secret": name }));
    v["title"] = title.into();
    v["description"] = description.into();
    v
}

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    const WEBHOOK: &[&str] = &["webhook"];
    const NTFY: &[&str] = &["ntfy"];
    const EMAIL: &[&str] = &["email"];
    let order = [
        "type",
        "url",
        "urlSecret",
        "signingSecret",
        "server",
        "topic",
        "token",
        "priority",
        "tags",
        "host",
        "port",
        "tls",
        "username",
        "password",
        "from",
        "to",
        "watchers",
    ];
    let props = json!({
        "type": titled(string_enum(&["webhook", "ntfy", "email"]), "Type", "How the channel delivers: a webhook POST, an ntfy publish or an email over SMTP."),
        "url": types(unset({
            let mut v = titled(string(), "URL", "The http or https URL the envelope is posted to. A URL that carries a user name, a password, a query or a fragment is refused, so name such a URL with the URL secret instead. A private address requires --outbound-allow-private.");
            v["format"] = "uri".into();
            v
        }, "the URL secret"), WEBHOOK),
        "urlSecret": types(secret_ref("URL secret", "A secret that holds the whole URL, for a URL that carries a credential, in place of URL.", "The secret that holds the URL. Set either URL or this."), WEBHOOK),
        "signingSecret": types(unset(secret_ref("Signing secret", "A secret that holds the key of the Standard Webhooks signature. Without it, requests are not signed.", "The secret that holds the signing key. A value that starts with whsec_ is decoded from Base64."), "not signed"), WEBHOOK),
        "server": types(default({
            let mut v = titled(string(), "Server", "The ntfy server's base URL.");
            v["format"] = "uri".into();
            v
        }, json!(crate::notify::config::NTFY_SH)), NTFY),
        "topic": types({
            let mut v = titled(string(), "Topic", "The topic to publish to, 1 to 64 letters, digits, - and _. Anyone who guesses a topic on a public server can read it, so use a token with a reserved topic or a server of your own.");
            v["pattern"] = "^[A-Za-z0-9_-]{1,64}$".into();
            v
        }, NTFY),
        "token": types(unset(secret_ref("Access token", "A secret that holds an ntfy access token, sent as a bearer token.", "The secret that holds the token."), "no token"), NTFY),
        "priority": types(unset(bounded(titled(int(), "Priority", "The ntfy priority from 1 to 5."), 1.0, 5.0), "4 for warnings, 5 for critical events, else 3"), NTFY),
        "tags": types({
            let mut v = titled(strings(), "Tags", "Tags added to the event type's own tag, at most 10 of 1 to 64 characters without commas.");
            v["items"]["minLength"] = json!(1);
            v["items"]["maxLength"] = json!(64);
            v
        }, NTFY),
        "host": types(titled(string(), "Host", "The SMTP server's host name or IP address. It passes the outbound policy's host check."), EMAIL),
        "port": types(unset(bounded(titled(int(), "Port", "The SMTP server's port."), 1.0, 65535.0), "587 with STARTTLS, 465 with TLS"), EMAIL),
        "tls": types(default(titled(string_enum(&["starttls", "tls"]), "TLS", "STARTTLS on the submission port, or TLS from the first byte. A connection without TLS is refused."), json!("starttls")), EMAIL),
        "username": types(unset(titled(string(), "User name", "The SASL PLAIN user name. It needs a password."), "no authentication"), EMAIL),
        "password": types(unset(secret_ref("Password", "A secret that holds the SMTP password, sent with SASL PLAIN. It needs a user name.", "The secret that holds the password."), "no authentication"), EMAIL),
        "from": types(titled(string(), "From", "The sender's mail address."), EMAIL),
        "to": types({
            let mut v = titled(strings(), "To", "The recipients' mail addresses for the events the routes send, at most 20. A channel that carries watches may have none, and the routes cannot then name it.");
            v["items"]["minLength"] = json!(1);
            v
        }, EMAIL),
        "watchers": types(default(titled(boolean(), "Watchers", "The channel also mails the owners of watches on entities, each at the address that the auth configuration or the identity provider verified, never through the routes."), json!(false)), EMAIL),
    });
    let mut channel = ordered(&["type"], &order, props);
    channel["title"] = "Channel".into();
    put(
        "NotificationChannelSettings",
        doc(
            channel,
            "One channel of the `notifications` kind. `type` decides the members it takes, and `x-sparkles-kinds` lists the types each member applies to. A credential is always named by a secret reference and never held inline.",
            "notifications",
        ),
    );

    let delivery = {
        let mut v = ordered(
            &[],
            &["attempts", "backoffSecs", "maxBackoffSecs", "timeoutSecs"],
            json!({
                "attempts": default(bounded(titled(int(), "Attempts", "Attempts of one delivery, the first included."), 1.0, 10.0), json!(5)),
                "backoffSecs": default(seconds("Backoff", "The wait before the second attempt, which doubles for each later one.", 86_400.0), json!(30)),
                "maxBackoffSecs": default(seconds("Longest backoff", "The longest wait between two attempts, at least the backoff.", 86_400.0), json!(1800)),
                "timeoutSecs": default(seconds("Timeout", "How long one attempt may take.", 300.0), json!(10)),
            }),
        );
        v["title"] = "Delivery".into();
        v["description"] = "The retries and timeout of each delivery.".into();
        v
    };
    let watches = {
        let mut v = ordered(
            &[],
            crate::notify::config::WATCH_MEMBERS,
            json!({
                "maxAncestors": default(bounded(titled(int(), "Ancestors per walk", "The most ancestors one upward walk of a subtree watch visits."), 1.0, 1_000_000.0), json!(10_000)),
                "largeCommit": default(bounded(titled(int(), "Large commit", "The most members of one watch a commit can name before its match becomes one summary entry."), 1.0, 1_000_000.0), json!(1_000)),
                "identityMaxAgeDays": default(bounded(titled(int(), "Identity age", "Days after an OIDC or proxy owner's last sign-in after which its watches are told only in the web UI."), 1.0, 3650.0), json!(30)),
                "batchSecs": default(bounded(titled(int(), "Batch window", "Seconds that gather one owner's matches in one dataset into one message."), 10.0, 86_400.0), json!(300)),
            }),
        );
        v["title"] = "Watches".into();
        v["description"] = "The bounds of watches on entities.".into();
        v
    };
    let mut settings = ordered(
        &[],
        crate::notify::config::MEMBERS,
        json!({
            "enabled": default(titled(boolean(), "Enabled", "Deliver events to the channels the routes name. A test send works either way."), json!(false)),
            "server": unset({
                let mut v = titled(string(), "Server name", "The server's name in each notification.");
                v["maxLength"] = json!(200);
                v
            }, "the host name"),
            "baseUrl": unset({
                let mut v = titled(string(), "Base URL", "The URL the web UI is reached at, which makes the links in notifications absolute.");
                v["format"] = "uri".into();
                v
            }, "relative links"),
            "repeatEvery": default({
                let mut v = titled(string(), "Repeat every", "The shortest time between two notifications about one lasting condition, as a duration of at least 1m such as 12h or 1d.");
                v["pattern"] = "^[0-9]+(\\.[0-9]+)?[smhdwy]$".into();
                v
            }, json!("1d")),
            "channels": {
                "type": "object",
                "title": "Channels",
                "description": "Channels by name, at most 32. Names can contain only letters, digits and the characters _ - .",
                "propertyNames": { "pattern": "^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$" },
                "additionalProperties": sref("NotificationChannelSettings"),
            },
            "routes": {
                "type": "object",
                "title": "Routes",
                "description": "The channels each event type goes to. A key is an event type, a prefix ending in .* or *, and an event goes to the union of the channels of every key that matches it. Each route is one field.",
                "propertyNames": { "pattern": "^(\\*|[a-z0-9-]+(\\.[a-z0-9-]+)*(\\.\\*)?)$" },
                "additionalProperties": titled(strings(), "Channels", "The channels this route sends to."),
            },
            "delivery": delivery,
            "watches": watches,
        }),
    );
    settings["title"] = "Notification configuration".into();
    put(
        "NotificationsSettings",
        doc(
            settings,
            "The `notifications` kind of `/$/server/settings/notifications` and of the settings file's `server.notifications`: whether events are delivered, the channels, the routes from event types to channels, the repeat interval and the retries. The web UI builds its form from this schema.",
            "notifications",
        ),
    );
}
