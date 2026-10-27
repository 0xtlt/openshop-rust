//! Signatures openshop makes itself (OAuth states, operator sessions), and
//! what it checks of a signed app proxy request beyond its signature, which
//! `shopify_api` verifies ([`ShopifyApp::verify_app_proxy`]).
//!
//! [`ShopifyApp::verify_app_proxy`]: shopify_api::oauth::ShopifyApp::verify_app_proxy

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

/// Whether the signed message of an app proxy request can only be cut into
/// its pairs one way, as far as who the request is for goes.
///
/// Shopify signs the pairs written one after the other, without a separator:
/// `logged_in_customer_id=path_prefix=/apps/xshop=a.myshopify.com…`. A
/// visitor who adds parameters of their own could make a message that also
/// reads as another shop or another customer. So no `=` stands in a key or a
/// value, the parameters of Shopify appear once with the values they need,
/// and no key of the visitor, after the value before it, ends with the name
/// of a parameter of Shopify.
pub(crate) fn proxy_params_are_unambiguous(params: &[(String, String)]) -> bool {
    // `shopify_api` refuses a repeated `signature` itself.
    let repeated = |name: &str| params.iter().filter(|(key, _)| key == name).count() > 1;
    if params
        .iter()
        .any(|(key, value)| key.is_empty() || key.contains('=') || value.contains('='))
        || PROXY_OWN_PARAMETERS.iter().any(|name| repeated(name))
    {
        return false;
    }

    let pairs = proxy_pairs(params.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    let mut previous = "";
    for (key, value) in &pairs {
        let sound = match *key {
            "timestamp" => !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
            "path_prefix" => {
                !value.is_empty()
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"/_-".contains(&byte))
            }
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

/// The shop of an app proxy request, exactly a `<name>.myshopify.com` domain.
pub(crate) fn proxy_shop(params: &[(String, String)]) -> Option<String> {
    let shop = &params.iter().find(|(key, _)| key == "shop")?.1;
    let domain = shop.to_ascii_lowercase();
    (strict_shop_domain(&domain).as_deref() == Some(domain.as_str())).then_some(domain)
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use shopify_api::oauth::ShopifyApp;

    use super::*;

    const SECRET: &str = "test-hmac-secret";

    /// A timestamp `shopify_api` takes as fresh while the tests run.
    static TIMESTAMP: LazyLock<String> = LazyLock::new(|| chrono::Utc::now().timestamp().to_string());

    /// The message Shopify signs for an app proxy request.
    fn proxy_message<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
        proxy_pairs(pairs)
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect()
    }

    fn query_of(params: &[(String, String)]) -> String {
        serde_urlencoded::to_string(params).unwrap()
    }

    /// What the proxy routes check: the pairs, then their signature with `shopify_api`.
    fn verify_proxy_signature(params: &[(String, String)], secret: &str) -> bool {
        proxy_params_are_unambiguous(params)
            && ShopifyApp::new("api-key", secret)
                .verify_app_proxy(&query_of(params))
                .is_ok()
    }

    fn pairs(params: &[(&str, &str)]) -> Vec<(String, String)> {
        params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    /// The pairs as Shopify sends them: with the signature of the others.
    fn signed_pairs(params: &[(&str, &str)], secret: &str) -> Vec<(String, String)> {
        let mut signed = pairs(params);
        let signature = ShopifyApp::new("api-key", secret).sign_app_proxy(&query_of(&signed)).unwrap();
        signed.push(("signature".to_string(), signature));
        signed
    }

    fn verifies(params: &[(&str, &str)]) -> bool {
        verify_proxy_signature(&signed_pairs(params, SECRET), SECRET)
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
        let timestamp = TIMESTAMP.clone();
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
        assert!(!verify_proxy_signature(&signed, SECRET));
        assert!(!verify_proxy_signature(&resplit, SECRET));
    }

    #[test]
    fn a_proxy_parameter_with_an_equal_sign_is_refused_whatever_its_timestamp() {
        let timestamp = TIMESTAMP.clone();
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
        let timestamp = TIMESTAMP.clone();
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
        assert!(!verify_proxy_signature(&twice, SECRET));
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
        let timestamp = TIMESTAMP.clone();
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
        let timestamp = TIMESTAMP.clone();
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
            if !verify_proxy_signature(&presented, SECRET) || proxy_shop(&presented).is_none() {
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
        let mut params = vec![("logged_in_customer_id", customer), ("path_prefix", prefix), ("shop", shop), ("timestamp", TIMESTAMP.as_str())];
        params.extend_from_slice(rest);
        params
    }

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
                ("timestamp", TIMESTAMP.as_str()),
            ],
            SECRET,
        );
        assert_eq!(
            proxy_message(reviewed.iter().copied()),
            proxy_message(forged.iter().map(|(key, value)| (key.as_str(), value.as_str())))
        );
        assert!(!verify_proxy_signature(&forged, SECRET));
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
                assert!(verify_proxy_signature(&signed, SECRET), "{params:?}");
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
                ("timestamp", TIMESTAMP.as_str()),
            ],
            SECRET,
        );
        let mut presented: Vec<_> = moved[..5].to_vec();
        presented.push(original.last().unwrap().clone());
        assert!(verify_proxy_signature(&presented, SECRET));
    }
}
