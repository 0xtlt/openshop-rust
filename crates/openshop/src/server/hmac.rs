//! Shopify request signatures.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use super::shop_domain::strict_shop_domain;

type HmacSha256 = Hmac<Sha256>;

pub(crate) fn hmac_sha256(secret: &str, message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// Compares two byte strings without revealing where they differ.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn sorted_message(query: &IndexMap<String, String>, excluded: &str, separator: &str) -> String {
    let mut keys: Vec<&String> = query.keys().filter(|key| key.as_str() != excluded).collect();
    keys.sort();
    keys.into_iter()
        .map(|key| format!("{key}={}", query[key]))
        .collect::<Vec<_>>()
        .join(separator)
}

/// Verify Shopify OAuth callback HMAC.
/// Shopify signs the query string (excluding the `hmac` param) with the app secret.
pub fn verify_query_hmac(query: &IndexMap<String, String>, secret: &str) -> bool {
    if secret.trim().is_empty() {
        return false;
    }
    let Some(hmac) = query.get("hmac").filter(|hmac| !hmac.is_empty()) else {
        return false;
    };

    let computed = hex::encode(hmac_sha256(secret, sorted_message(query, "hmac", "&").as_bytes()));
    constant_time_eq(computed.as_bytes(), hmac.as_bytes())
}

/// Verify Shopify webhook HMAC.
/// Shopify sends the HMAC in the `X-Shopify-Hmac-Sha256` header. The signature
/// covers the bytes of the body as they were sent, which need not be UTF-8.
pub fn verify_webhook_hmac(body: &[u8], hmac: &str, secret: &str) -> bool {
    if secret.trim().is_empty() {
        return false;
    }
    let computed = STANDARD.encode(hmac_sha256(secret, body));
    constant_time_eq(computed.as_bytes(), hmac.as_bytes())
}

/// How far, in seconds, the `timestamp` of a signed app proxy request may be
/// from now, on either side to allow for clock skew. A signed URL is not a
/// credential for longer than that.
pub(crate) const PROXY_SIGNATURE_FRESHNESS: u64 = 300;

/// Parameters Shopify itself sets on an app proxy request, besides
/// `signature`. Each appears once: a copy added by the sender would be joined
/// with the real one in the signed message, and read as another value by the
/// handler.
const PROXY_OWN_PARAMETERS: [&str; 4] = ["logged_in_customer_id", "path_prefix", "shop", "timestamp"];

/// The pairs Shopify signs for an app proxy request: every parameter other
/// than `signature`, the values of a repeated key joined by `,`, sorted as the
/// `key=value` strings they are written as.
fn proxy_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<(&'a str, String)> {
    let mut grouped: IndexMap<&str, Vec<&str>> = IndexMap::new();
    for (key, value) in pairs {
        if key != "signature" {
            grouped.entry(key).or_default().push(value);
        }
    }
    let mut pairs: Vec<(&str, String)> = grouped
        .into_iter()
        .map(|(key, values)| (key, values.join(",")))
        .collect();
    pairs.sort_by_cached_key(|(key, value)| format!("{key}={value}"));
    pairs
}

/// The message Shopify signs: those pairs written `key=value` with no separator.
fn proxy_message<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    proxy_pairs(pairs)
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Hex signature of an app proxy query (sorted `key=value` pairs, no separator).
pub fn proxy_signature(query: &IndexMap<String, String>, secret: &str) -> String {
    let message = proxy_message(query.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    hex::encode(hmac_sha256(secret, message.as_bytes()))
}

/// Whether the parameters can only be read one way from the message that is
/// signed, as far as the parameters of Shopify go.
///
/// The pairs are glued without separator, so a signature stands for every way
/// to cut the same text in `key=value` pairs. Nothing can be said of what the
/// visitor chose to send, but the cuts are kept from moving `shop`,
/// `timestamp` or `logged_in_customer_id`, whatever the visitor sent:
///
/// - No key or value holds a `=` and no key is empty. Every `=` of the message
///   is then the one of a pair, and the cuts that remain are those of the
///   text between two of them, a value followed by a key.
/// - The parameters of Shopify appear once. A key that is not theirs does not
///   leave the text it is cut from ending with one of their names: Shopify
///   signed that name there as a key, so another cut would make it a key
///   again, or lose it. Since it appears once, no name is made twice.
/// - `timestamp` is digits, and the key after it sorts after it, so it cannot
///   begin with a digit: no digit moves from key to value or back. The same
///   goes for `logged_in_customer_id`, read as a customer when it is digits.
///   `shop` is a whole domain, see [`proxy_shop`], and no such domain is the
///   beginning of a longer one.
///
/// `path_prefix` is held to a plain path, but its end can still move into the
/// key that follows: it is no identity, and nothing trusts it.
///
/// This relies on Shopify sending all of its parameters, `logged_in_customer_id`
/// included (empty for a visitor without account).
fn proxy_params_are_unambiguous(params: &[(String, String)]) -> bool {
    let repeated = |name: &str| params.iter().filter(|(key, _)| key == name).count() > 1;
    if params
        .iter()
        .any(|(key, value)| key.is_empty() || key.contains('=') || value.contains('='))
        || repeated("signature")
        || PROXY_OWN_PARAMETERS.iter().any(|name| repeated(name))
    {
        return false;
    }

    let pairs = proxy_pairs(params.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    let mut previous = "";
    for (key, value) in &pairs {
        let sound = match *key {
            "timestamp" => !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
            "path_prefix" => !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"/_-".contains(&byte)),
            "logged_in_customer_id" | "shop" => true,
            _ => {
                let text = format!("{previous}{key}");
                !PROXY_OWN_PARAMETERS.iter().any(|name| text.ends_with(name))
            }
        };
        if !sound {
            return false;
        }
        previous = value;
    }
    true
}

/// The shop of an app proxy request, when the `shop` parameter is exactly a
/// `<name>.myshopify.com` domain (in any case). No URL, no user, no port, no
/// blank: no domain of this form starts another, so the signed text cannot be
/// cut to make a longer one, nor one that [`normalize_shop_domain`] would read
/// as another shop.
///
/// [`normalize_shop_domain`]: super::shop_domain::normalize_shop_domain
pub(crate) fn proxy_shop(params: &[(String, String)]) -> Option<String> {
    let shop = &params.iter().find(|(key, _)| key == "shop")?.1;
    let domain = shop.to_ascii_lowercase();
    (strict_shop_domain(&domain).as_deref() == Some(domain.as_str())).then_some(domain)
}

/// Whether `timestamp` is a Unix time within [`PROXY_SIGNATURE_FRESHNESS`] of `now`.
fn proxy_timestamp_is_fresh(timestamp: &str, now: i64) -> bool {
    timestamp
        .parse::<i64>()
        .is_ok_and(|timestamp| now.abs_diff(timestamp) <= PROXY_SIGNATURE_FRESHNESS)
}

/// Verify the `signature` param Shopify adds to app proxy requests.
///
/// `params` are the query pairs as received, repeated keys included, and `now`
/// is the Unix time in seconds. A request is refused when its `timestamp` is
/// missing or too far from `now`, so that a signed URL cannot be replayed
/// later, and when its parameters could be split another way, see
/// [`proxy_params_are_unambiguous`]. The signature covers neither the path, nor
/// the method, nor the body.
pub fn verify_proxy_signature(params: &[(String, String)], secret: &str, now: i64) -> bool {
    if secret.trim().is_empty() || !proxy_params_are_unambiguous(params) {
        return false;
    }
    let value = |name: &str| params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    let Some(signature) = value("signature").filter(|signature| !signature.is_empty()) else {
        return false;
    };
    if !value("timestamp").is_some_and(|timestamp| proxy_timestamp_is_fresh(timestamp, now)) {
        return false;
    }
    let Ok(provided) = hex::decode(signature) else {
        return false;
    };
    let message = proxy_message(params.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    constant_time_eq(&hmac_sha256(secret, message.as_bytes()), &provided)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SECRET: &str = "test-hmac-secret";

    pub(crate) fn sign_query(params: &[(&str, &str)], secret: &str) -> IndexMap<String, String> {
        let mut query: IndexMap<String, String> = params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let hmac = hex::encode(hmac_sha256(secret, sorted_message(&query, "hmac", "&").as_bytes()));
        query.insert("hmac".into(), hmac);
        query
    }

    pub(crate) fn sign_body(body: &str, secret: &str) -> String {
        STANDARD.encode(hmac_sha256(secret, body.as_bytes()))
    }

    #[test]
    fn valid_signed_query_returns_true() {
        let query = sign_query(&[("shop", "test.myshopify.com"), ("timestamp", "123456")], SECRET);
        assert!(verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn missing_hmac_returns_false() {
        let query: IndexMap<String, String> = [("shop".to_string(), "test.myshopify.com".to_string())].into();
        assert!(!verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn empty_secret_returns_false_even_when_query_is_signed_with_an_empty_secret() {
        let query = sign_query(&[("shop", "test.myshopify.com"), ("timestamp", "123456")], "");
        assert!(!verify_query_hmac(&query, ""));
        assert!(!verify_query_hmac(&query, "   "));
    }

    #[test]
    fn wrong_hmac_returns_false() {
        let mut query = sign_query(&[("shop", "test.myshopify.com")], SECRET);
        query.insert("hmac".into(), "deadbeef".into());
        assert!(!verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn param_order_does_not_matter() {
        let query = sign_query(&[("z", "1"), ("a", "2"), ("m", "3")], SECRET);
        assert!(verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn valid_body_hmac_returns_true() {
        let body = r#"{"id":123,"name":"order"}"#;
        assert!(verify_webhook_hmac(body.as_bytes(), &sign_body(body, SECRET), SECRET));
    }

    #[test]
    fn wrong_body_hmac_returns_false() {
        assert!(!verify_webhook_hmac(br#"{"id":123}"#, "wrong-hmac", SECRET));
    }

    #[test]
    fn different_body_returns_false() {
        let hmac = sign_body(r#"{"id":456}"#, SECRET);
        assert!(!verify_webhook_hmac(br#"{"id":123}"#, &hmac, SECRET));
    }

    #[test]
    fn the_signature_covers_the_bytes_that_were_sent() {
        // Not UTF-8: a lossy conversion would replace the bytes with U+FFFD.
        let body: &[u8] = b"{\"note\":\"caf\xe9\"}";
        let hmac = STANDARD.encode(hmac_sha256(SECRET, body));
        assert!(verify_webhook_hmac(body, &hmac, SECRET));
        // What a lossy conversion of that body would have been signed as.
        let mangled = String::from_utf8_lossy(body).into_owned();
        assert_ne!(mangled.as_bytes(), body);
        assert!(!verify_webhook_hmac(mangled.as_bytes(), &hmac, SECRET));
        let hmac_of_mangled = sign_body(&mangled, SECRET);
        assert!(!verify_webhook_hmac(body, &hmac_of_mangled, SECRET));
    }

    #[test]
    fn empty_secret_returns_false_even_when_body_is_signed_with_an_empty_secret() {
        let body = r#"{"id":123}"#;
        let hmac = sign_body(body, "");
        assert!(!verify_webhook_hmac(body.as_bytes(), &hmac, ""));
        assert!(!verify_webhook_hmac(body.as_bytes(), &hmac, "   "));
    }

    const NOW: i64 = 1_780_000_000;

    fn pairs(params: &[(&str, &str)]) -> Vec<(String, String)> {
        params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    /// The pairs as Shopify sends them: with the signature of the others.
    fn signed_pairs(params: &[(&str, &str)], secret: &str) -> Vec<(String, String)> {
        let mut signed = pairs(params);
        let message = proxy_message(params.iter().copied());
        signed.push(("signature".to_string(), hex::encode(hmac_sha256(secret, message.as_bytes()))));
        signed
    }

    fn verifies(params: &[(&str, &str)]) -> bool {
        verify_proxy_signature(&signed_pairs(params, SECRET), SECRET, NOW)
    }

    fn shop_at(timestamp: i64, now: i64) -> bool {
        let timestamp = timestamp.to_string();
        let params = [("shop", "test.myshopify.com"), ("timestamp", timestamp.as_str())];
        verify_proxy_signature(&signed_pairs(&params, SECRET), SECRET, now)
    }

    #[test]
    fn verifies_proxy_signatures() {
        let params = [
            ("shop", "test.myshopify.com"),
            ("path_prefix", "/apps/openshop"),
            ("timestamp", "1780000000"),
        ];
        let signed = signed_pairs(&params, SECRET);
        assert!(verify_proxy_signature(&signed, SECRET, NOW));
        assert!(!verify_proxy_signature(&signed, "other-secret", NOW));
        assert!(!verify_proxy_signature(&signed, "", NOW));
        assert!(!verify_proxy_signature(&signed, "  ", NOW));

        let mut bad = signed.clone();
        bad[3].1.push_str("zz");
        assert!(!verify_proxy_signature(&bad, SECRET, NOW));
        bad[3].1 = String::new();
        assert!(!verify_proxy_signature(&bad, SECRET, NOW));

        let mut other_shop = signed.clone();
        other_shop[0].1 = "other.myshopify.com".to_string();
        assert!(!verify_proxy_signature(&other_shop, SECRET, NOW));

        // The order of the parameters does not matter, nor does where the signature is.
        let mut shuffled = signed.clone();
        shuffled.reverse();
        assert!(verify_proxy_signature(&shuffled, SECRET, NOW));

        // The map form signs the same message.
        let query: IndexMap<String, String> = signed[..3].iter().cloned().collect();
        assert_eq!(proxy_signature(&query, SECRET), signed[3].1);
    }

    #[test]
    fn a_proxy_signature_is_only_fresh_for_a_few_minutes() {
        let window = PROXY_SIGNATURE_FRESHNESS as i64;
        assert_eq!(window, 300);

        // Either side of now, within the window.
        assert!(shop_at(NOW, NOW));
        assert!(shop_at(NOW - window, NOW));
        assert!(shop_at(NOW + window, NOW));
        // Replayed later, or signed by a clock that is ahead.
        assert!(!shop_at(NOW - window - 1, NOW));
        assert!(!shop_at(NOW + window + 1, NOW));
        assert!(!shop_at(NOW - 86_400, NOW));
        assert!(!shop_at(i64::MIN, NOW));
        assert!(!shop_at(i64::MAX, NOW));
    }

    #[test]
    fn a_proxy_signature_needs_a_numeric_timestamp() {
        let shop = ("shop", "test.myshopify.com");
        assert!(!verifies(&[shop]));
        for timestamp in ["", "soon", "1780000000.5", "0x6a0b3c00", " 1780000000"] {
            assert!(!verifies(&[shop, ("timestamp", timestamp)]), "{timestamp}");
        }
        assert!(verifies(&[shop, ("timestamp", "1780000000")]));
    }

    #[test]
    fn a_parameter_sent_several_times_is_signed_with_its_values_joined() {
        // Shopify signs `extra=1,2` for `extra=1&extra=2`.
        let joined = signed_pairs(
            &[("shop", "test.myshopify.com"), ("extra", "1,2"), ("timestamp", "1780000000")],
            SECRET,
        );
        let signature = joined[3].clone();
        let mut repeated = pairs(&[
            ("shop", "test.myshopify.com"),
            ("extra", "1"),
            ("timestamp", "1780000000"),
            ("extra", "2"),
        ]);
        repeated.push(signature);
        assert!(verify_proxy_signature(&joined, SECRET, NOW));
        assert!(verify_proxy_signature(&repeated, SECRET, NOW));

        // The order of the values is part of what is signed.
        repeated[1].1 = "2".to_string();
        repeated[3].1 = "1".to_string();
        assert!(!verify_proxy_signature(&repeated, SECRET, NOW));
    }

    #[test]
    fn the_proxy_message_sorts_the_pairs_as_strings() {
        // `a-b=…` comes before `a=…`, which sorting the keys alone would not say.
        let message = proxy_message([("a", "1"), ("a-b", "2"), ("b", "3")]);
        assert_eq!(message, "a-b=2a=1b=3");
        assert_eq!(proxy_message([("signature", "x"), ("k", "v")]), "k=v");
    }

    #[test]
    fn the_pairs_of_a_proxy_signature_cannot_be_split_again() {
        let timestamp = NOW.to_string();
        // The request for a shop of the sender's, whose parameter `m` carries the
        // text of the request for another shop.
        let note = format!("1path_prefix=/apps/xshop=victim.myshopify.comtimestamp={timestamp}");
        let own = [
            ("m", note.as_str()),
            ("path_prefix", "/apps/x"),
            ("shop", "attacker.myshopify.com"),
            ("timestamp", timestamp.as_str()),
        ];
        // The same text, cut so that the shop is the victim's.
        let tail = format!("{timestamp}path_prefix=/apps/xshop=attacker.myshopify.comtimestamp={timestamp}");
        let forged = [
            ("m", "1"),
            ("path_prefix", "/apps/x"),
            ("shop", "victim.myshopify.com"),
            ("timestamp", tail.as_str()),
        ];
        // They read the same to the signature.
        assert_eq!(proxy_message(own), proxy_message(forged));

        let signed = signed_pairs(&own, SECRET);
        let mut resplit = pairs(&forged);
        resplit.push(signed[4].clone());
        assert!(!verify_proxy_signature(&signed, SECRET, NOW));
        assert!(!verify_proxy_signature(&resplit, SECRET, NOW));
    }

    #[test]
    fn a_proxy_parameter_with_an_equal_sign_is_refused_whatever_its_timestamp() {
        let timestamp = NOW.to_string();
        let shop = ("shop", "a.myshopify.com");
        let at = ("timestamp", timestamp.as_str());
        assert!(verifies(&[shop, at, ("q", "shoes")]));
        assert!(!verifies(&[shop, at, ("q", "a=b")]));
        assert!(!verifies(&[shop, at, ("q=a", "b")]));
        assert!(!verifies(&[shop, at, ("", "b")]));
        assert!(!verifies(&[("shop", "a=b.myshopify.com"), at]));
    }

    #[test]
    fn a_parameter_of_shopify_is_not_repeated() {
        let timestamp = NOW.to_string();
        let shop = ("shop", "a.myshopify.com");
        let at = ("timestamp", timestamp.as_str());
        assert!(verifies(&[shop, at, ("tag", "a"), ("tag", "b")]));
        assert!(!verifies(&[shop, at, ("shop", "b.myshopify.com")]));
        assert!(!verifies(&[shop, at, at]));
        for name in ["path_prefix", "logged_in_customer_id"] {
            assert!(verifies(&[shop, at, (name, "1")]), "{name}");
            assert!(!verifies(&[shop, at, (name, "1"), (name, "2")]), "{name}");
        }

        // A second `signature` leaves no telling which one signs.
        let mut twice = signed_pairs(&[shop, at], SECRET);
        twice.push(twice[2].clone());
        assert!(!verify_proxy_signature(&twice, SECRET, NOW));
    }

    #[test]
    fn the_shop_of_a_proxy_request_is_exactly_a_shop_domain() {
        let shop = |value: &str| proxy_shop(&pairs(&[("shop", value)]));
        assert_eq!(shop("a.myshopify.com").as_deref(), Some("a.myshopify.com"));
        assert_eq!(shop("Test-1.MyShopify.com").as_deref(), Some("test-1.myshopify.com"));
        for value in [
            "",
            "a",
            " a.myshopify.com",
            "a.myshopify.com ",
            "https://a.myshopify.com",
            "a.myshopify.com/x",
            "a.myshopify.com:443",
            "a.myshopify.comt@b.myshopify.com",
            "a.myshopify.com.evil.com",
            "a.b.myshopify.com",
        ] {
            assert_eq!(shop(value), None, "{value:?}");
        }
        assert_eq!(proxy_shop(&[]), None);
    }

    #[test]
    fn a_key_cannot_end_where_a_parameter_of_shopify_does() {
        let timestamp = NOW.to_string();
        let shop = ("shop", "a.myshopify.com");
        let at = ("timestamp", timestamp.as_str());
        assert!(verifies(&[shop, at, ("customer_id", "1"), ("id", "2"), ("p", "3")]));
        // The name is the end of the key.
        assert!(!verifies(&[shop, at, ("xshop", "1")]));
        assert!(!verifies(&[shop, at, ("my_logged_in_customer_id", "1")]));
        // The name is the end of the value before and of the key.
        assert!(!verifies(&[shop, at, ("a", "1logged_in_customer_"), ("id", "2")]));
        assert!(!verifies(&[shop, at, ("a", "sho"), ("p", "2")]));
        assert!(verifies(&[shop, at, ("a", "sho"), ("q", "2")]));
    }

    #[test]
    fn the_parameters_of_shopify_have_the_values_they_need() {
        let timestamp = NOW.to_string();
        let shop = ("shop", "a.myshopify.com");
        let at = ("timestamp", timestamp.as_str());
        assert!(verifies(&[shop, at, ("path_prefix", "/apps/openshop-2"), ("logged_in_customer_id", "")]));
        for prefix in ["", "/apps/<b>", "/apps/x y", "/apps/x?y"] {
            assert!(!verifies(&[shop, at, ("path_prefix", prefix)]), "{prefix:?}");
        }
    }

    type Cut<'a> = [(&'a str, &'a str)];

    /// Every way to cut the text of a message again in sorted `key=value` pairs,
    /// as long as a `=` is only the one of a pair and no key is empty.
    fn recut(message: &str, found: &mut dyn FnMut(&Cut)) {
        fn line<'a>(pair: &(&'a str, &'a str)) -> impl Iterator<Item = u8> + 'a {
            pair.0.bytes().chain(std::iter::once(b'=')).chain(pair.1.bytes())
        }
        fn from<'a>(
            texts: &[&'a str],
            at: usize,
            key: &'a str,
            pairs: &mut Vec<(&'a str, &'a str)>,
            found: &mut dyn FnMut(&Cut),
        ) {
            let ascending = |pairs: &[(&str, &str)], pair: &(&str, &str)| {
                pairs.last().is_none_or(|last| line(last).cmp(line(pair)).is_lt())
            };
            if at == texts.len() - 1 {
                let pair = (key, texts[at]);
                if ascending(pairs, &pair) {
                    pairs.push(pair);
                    found(pairs);
                    pairs.pop();
                }
                return;
            }
            let text = texts[at];
            for cut in 0..text.len() {
                let pair = (key, &text[..cut]);
                if ascending(pairs, &pair) {
                    pairs.push(pair);
                    from(texts, at + 1, &text[cut..], pairs, found);
                    pairs.pop();
                }
            }
        }
        let texts: Vec<&str> = message.split('=').collect();
        if texts.len() > 1 && !texts[0].is_empty() {
            from(&texts, 1, texts[0], &mut Vec::new(), found);
        }
    }

    /// A customer, as the handler reads `logged_in_customer_id`.
    fn customer(params: &[(String, String)]) -> Option<String> {
        let (_, value) = params.iter().find(|(key, _)| key == "logged_in_customer_id")?;
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())).then(|| value.clone())
    }

    fn value_of(params: &[(String, String)], name: &str) -> Option<String> {
        params.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone())
    }

    /// Signs `params` as Shopify would, then tries every other cut of what was
    /// signed with the same signature. Returns how many were accepted; none
    /// may change who the request is for, or who makes it.
    fn accepted_recuts(params: &[(&str, &str)]) -> usize {
        let original = signed_pairs(params, SECRET);
        let signature = original.last().unwrap().clone();
        let message = proxy_message(params.iter().copied());
        let shop = proxy_shop(&original);
        let mut accepted = 0;
        recut(&message, &mut |pairs| {
            let mut presented = self::pairs(pairs);
            presented.push(signature.clone());
            if !verify_proxy_signature(&presented, SECRET, NOW) || proxy_shop(&presented).is_none() {
                return;
            }
            accepted += 1;
            let shown = format!("{params:?} read as {presented:?}");
            assert_eq!(proxy_shop(&presented), shop, "{shown}");
            assert_eq!(value_of(&presented, "timestamp"), value_of(&original, "timestamp"), "{shown}");
            // A visitor can be taken for no one, not for another one.
            let who = customer(&presented);
            assert!(who.is_none() || who == customer(&original), "{shown}");
        });
        accepted
    }

    /// The parameters of a request as Shopify sends them, then the visitor's own.
    fn shopify_request<'a>(customer: &'a str, shop: &'a str, prefix: &'a str, rest: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut params = vec![("logged_in_customer_id", customer), ("path_prefix", prefix), ("shop", shop), ("timestamp", TIMESTAMP)];
        params.extend_from_slice(rest);
        params
    }

    const TIMESTAMP: &str = "1780000000";

    #[test]
    fn no_other_cut_of_a_signed_proxy_query_changes_who_it_is_for() {
        // The reviewer's: a visitor without account, read as customer 42.
        let reviewed = shopify_request(
            "",
            "a.myshopify.com",
            "/apps/x",
            &[("1", "foo"), ("2logged_in_customer_id", "4"), ("2m", "o")],
        );
        let forged = signed_pairs(
            &[
                ("1", "foo2"),
                ("logged_in_customer_id", "42"),
                ("m", ""),
                ("ologged_in_customer_id", ""),
                ("path_prefix", "/apps/x"),
                ("shop", "a.myshopify.com"),
                ("timestamp", TIMESTAMP),
            ],
            SECRET,
        );
        assert_eq!(
            proxy_message(reviewed.iter().copied()),
            proxy_message(forged.iter().map(|(key, value)| (key.as_str(), value.as_str())))
        );
        assert!(!verify_proxy_signature(&forged, SECRET, NOW));
        assert_eq!(accepted_recuts(&reviewed), 0);

    }

    #[test]
    fn requests_of_shopify_with_the_usual_parameters_of_a_visitor_are_accepted() {
        for rest in [
            &[][..],
            &[("q", "shoes"), ("page", "2")],
            &[("extra", "1,2"), ("sort", "price"), ("id", "7"), ("customer_id", "9")],
            &[("shop_id", "5"), ("p", "x"), ("timestamp_ms", "1"), ("prefix", "y"), ("s", "sho")],
            &[("tag", "a/b"), ("qr", "1"), ("search", "logged in"), ("view", "list&grid")],
        ] {
            for customer in ["", "4", "42"] {
                let params = shopify_request(customer, "A.myshopify.com", "/apps/x", rest);
                let signed = signed_pairs(&params, SECRET);
                assert!(verify_proxy_signature(&signed, SECRET, NOW), "{params:?}");
                assert!(proxy_shop(&signed).is_some(), "{params:?}");
            }
        }
    }

    #[test]
    fn no_cut_of_any_signed_proxy_query_changes_who_it_is_for() {
        // Short pieces to cut anywhere, and the names of Shopify, whole or by the end.
        const PIECES: [&str; 24] = [
            "1", "2", "4", "o", "m", "_", "=", "/", ",", "p", "s", "h", "logged_in_customer_id", "logged_in_customer_id",
            "ogged_in_customer_id", "shop", "shop", "hop", "timestamp", "stamp", "path_prefix", "prefix", "id", "t",
        ];
        struct Rng(u64);
        impl Rng {
            fn next(&mut self, limit: usize) -> usize {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                (self.0 % limit as u64) as usize
            }

            fn text(&mut self, least: usize, most: usize) -> String {
                let length = least + self.next(most - least + 1);
                (0..length).map(|_| PIECES[self.next(PIECES.len())]).collect()
            }
        }
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);

        let (mut cases, mut accepted) = (0, 0);

        // Every request with up to three parameters of a visitor made of these:
        // the ones of the reviewed attack, and what could make a better one.
        let pairs = [
            ("1", "foo"),
            ("1", "4"),
            ("2m", "o"),
            ("m", "o"),
            ("2logged_in_customer_id", "4"),
            ("2logged_in_customer_id", "o"),
            ("2shop", "x"),
            ("1", "logged_in_customer_"),
            ("hop", "s"),
            ("t@b.myshopify.comta", "1"),
        ];
        for chosen in 1_u32..(1 << pairs.len()) {
            if chosen.count_ones() > 3 {
                continue;
            }
            let rest: Vec<(&str, &str)> = (0..pairs.len()).filter(|bit| chosen >> bit & 1 == 1).map(|bit| pairs[bit]).collect();
            accepted += accepted_recuts(&shopify_request("", "a.myshopify.com", "/apps/x", &rest));
            cases += 1;
        }

        let sweep = cases;
        while cases < sweep + 40 {
            let extras: Vec<(String, String)> = (1..=1 + rng.next(4))
                .map(|_| (rng.text(1, 3), rng.text(0, 3)))
                .filter(|(key, _)| !PROXY_OWN_PARAMETERS.contains(&key.as_str()) && key != "signature")
                .collect();
            let customer = ["", "4", "42"][rng.next(3)];
            let shop = ["a.myshopify.com", "b-2.myshopify.com"][rng.next(2)];
            let prefix = ["/apps/x", "/apps/lo", "/a/p"][rng.next(3)];
            let rest: Vec<(&str, &str)> = extras.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
            let params = shopify_request(customer, shop, prefix, &rest);
            // Duplicates of a key are one parameter, whose values are joined.
            if proxy_message(params.iter().copied()).len() > 90 {
                continue;
            }
            accepted += accepted_recuts(&params);
            cases += 1;
        }
        // Not a search that finds nothing because it asks for nothing.
        assert!(accepted > 100, "{accepted}");
    }

    #[test]
    fn the_end_of_path_prefix_can_still_move_into_the_next_key() {
        // Not an identity, and not read by OpenShop: the path stays a plain path.
        let params = shopify_request("", "a.myshopify.com", "/apps/x", &[("qr", "price")]);
        let original = signed_pairs(&params, SECRET);
        let moved = signed_pairs(
            &[
                ("logged_in_customer_id", ""),
                ("path_prefix", "/apps/xq"),
                ("r", "price"),
                ("shop", "a.myshopify.com"),
                ("timestamp", TIMESTAMP),
            ],
            SECRET,
        );
        let mut presented: Vec<_> = moved[..5].to_vec();
        presented.push(original.last().unwrap().clone());
        assert!(verify_proxy_signature(&presented, SECRET, NOW));
    }
}
