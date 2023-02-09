use crate::{error::*, ParsedRequest};
use base64::{engine::general_purpose::STANDARD, Engine};
use http::{
    header::{HeaderName, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT},
    HeaderValue,
};
use pest::Parser as _;
use pest_derive::Parser;
use snafu::ResultExt;
use std::str::FromStr;

#[derive(Debug, Parser)]
#[grammar = "src/curl.pest"]
pub struct CurlParser;

impl<'a> TryFrom<&'a str> for ParsedRequest<'a> {
    type Error = Error;

    fn try_from(s: &'a str) -> Result<Self> {
        parse_input(s)
    }
}

fn parse_input(input: &str) -> Result<ParsedRequest<'_>> {
    let pairs = CurlParser::parse(Rule::input, input).context(ParseRuleSnafu)?;
    let mut parsed = ParsedRequest::default();
    for pair in pairs {
        match pair.as_rule() {
            Rule::method => {
                let method = pair.as_str().parse().context(ParseMethodSnafu)?;
                parsed.method = method;
            }
            Rule::url => {
                let url = pair.as_str().parse().context(ParseUrlSnafu)?;
                parsed.url = url;
            }
            Rule::header => {
                let s = pair
                    .into_inner()
                    .next()
                    .expect("header string must be present")
                    .as_str();
                let mut kv = s.splitn(2, ':');
                let name = kv.next().expect("key must present").trim();
                let value = kv.next().expect("value must present").trim();
                parsed.headers.insert(
                    HeaderName::from_str(name).context(ParseHeaderNameSnafu)?,
                    HeaderValue::from_str(value).context(ParseHeaderValueSnafu)?,
                );
            }
            Rule::auth => {
                let s = pair
                    .into_inner()
                    .next()
                    .expect("header string must be present")
                    .as_str();
                let basic_auth = format!("Basic {}", STANDARD.encode(s.as_bytes()));
                parsed.headers.insert(
                    AUTHORIZATION,
                    basic_auth.parse().context(ParseHeaderValueSnafu)?,
                );
            }
            Rule::body => {
                let s = pair.as_str().trim();
                let s = remove_quote(s);
                parsed.body.push(s.into());
            }
            Rule::EOI => break,
            _ => unreachable!("Unexpected rule: {:?}", pair.as_rule()),
        }
    }

    if parsed.headers.get(CONTENT_TYPE).is_none() && !parsed.body.is_empty() {
        parsed.headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
    }
    if parsed.headers.get(ACCEPT).is_none() {
        parsed
            .headers
            .insert(ACCEPT, HeaderValue::from_static("*/*"));
    }
    parsed
        .headers
        .insert(USER_AGENT, HeaderValue::from_static("curl/7.84.0"));
    Ok(parsed)
}

impl<'a> ParsedRequest<'a> {
    pub fn body(&mut self) -> Option<String> {
        if self.body.is_empty() {
            return None;
        }

        match self.headers.get(CONTENT_TYPE) {
            Some(content_type) if content_type == "application/x-www-form-urlencoded" => {
                Some(self.form_urlencoded())
            }
            Some(content_type) if content_type == "application/json" => {
                self.body.pop().map(|v| v.into_owned())
            }
            v => unimplemented!("Unsupported content type: {:?}", v),
        }
    }

    fn form_urlencoded(&self) -> String {
        let mut encoded = form_urlencoded::Serializer::new(String::new());
        for item in &self.body {
            let mut kv = item.splitn(2, '=');
            let key = kv.next().expect("key must present");
            let value = kv.next().expect("value must present");
            encoded.append_pair(remove_quote(key), remove_quote(value));
        }
        encoded.finish()
    }
}

#[cfg(feature = "reqwest")]
impl<'a> From<ParsedRequest<'a>> for reqwest::RequestBuilder {
    fn from(mut parsed: ParsedRequest<'a>) -> Self {
        let body = parsed.body();
        let req = reqwest::Client::new()
            .request(parsed.method, parsed.url.to_string())
            .headers(parsed.headers);

        if let Some(body) = body {
            req.body(body)
        } else {
            req
        }
    }
}

fn remove_quote(s: &str) -> &str {
    match (&s[0..1], &s[s.len() - 1..]) {
        ("'", "'") => &s[1..s.len() - 1],
        ("\"", "\"") => &s[1..s.len() - 1],
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use http::{header::ACCEPT, Method};

    #[test]
    fn parse_curl_1_should_work() -> Result<()> {
        let input = r#"curl \
          -X PATCH \
          -d '{"visibility":"private"}' \
          -H "Accept: application/vnd.github+json" \
          -H "Authorization: Bearer <YOUR-TOKEN>"\
          -H "X-GitHub-Api-Version: 2022-11-28" \
          https://api.github.com/user/email/visibility "#;
        let parsed = ParsedRequest::try_from(input)?;
        assert_eq!(parsed.method, Method::PATCH);
        assert_eq!(
            parsed.url.to_string(),
            "https://api.github.com/user/email/visibility"
        );
        assert_eq!(
            parsed.headers.get(ACCEPT),
            Some(&HeaderValue::from_static("application/vnd.github+json"))
        );
        assert_eq!(parsed.body, vec!["{\"visibility\":\"private\"}"]);

        Ok(())
    }

    #[test]
    fn parse_curl_2_should_work() -> Result<()> {
        let input = r#"curl \
        -X POST \
        -H "Accept: application/vnd.github+json" \
        -H "Authorization: Bearer <YOUR-TOKEN>"\
        -H "X-GitHub-Api-Version: 2022-11-28" \
        https://api.github.com/user/emails \
        -d '{"emails":["octocat@github.com","mona@github.com","octocat@octocat.org"]}'"#;
        let parsed = ParsedRequest::try_from(input)?;
        assert_eq!(parsed.method, Method::POST);
        assert_eq!(parsed.url.to_string(), "https://api.github.com/user/emails");
        assert_eq!(
            parsed.headers.get(AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer <YOUR-TOKEN>"))
        );
        assert_eq!(
            parsed.body,
            vec![r#"{"emails":["octocat@github.com","mona@github.com","octocat@octocat.org"]}"#]
        );
        Ok(())
    }

    #[tokio::test]
    async fn parse_curl_3_should_work() -> Result<()> {
        let input = r#"curl https://api.stripe.com/v1/charges \
        -u sk_test_4eC39HqLyjWDarjtT1zdp7dc: \
        -H "Stripe-Version: 2022-11-15""#;

        let parsed = ParsedRequest::try_from(input)?;
        assert_eq!(parsed.method, Method::GET);
        assert_eq!(parsed.url.to_string(), "https://api.stripe.com/v1/charges");
        assert_eq!(
            parsed.headers.get(AUTHORIZATION),
            Some(&HeaderValue::from_static(
                "Basic c2tfdGVzdF80ZUMzOUhxTHlqV0Rhcmp0VDF6ZHA3ZGM6"
            ))
        );

        #[cfg(feature = "reqwest")]
        {
            let req: reqwest::RequestBuilder = parsed.into();
            let res = req.send().await?;
            assert_eq!(res.status(), 200);
            let _body = res.text().await?;
        }
        Ok(())
    }
}