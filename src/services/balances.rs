use crate::cache::cache_operations::RequestCached;
use crate::config::{balances_cache_duration, balances_request_timeout};
use crate::models::backend::balances::Balance as BalanceDto;
use crate::models::backend::chains::NativeCurrency;
use crate::models::backend::tokens::{TokenPrice, TokenPriceCore};
use crate::models::service::balances::{Balance, Balances};
use crate::providers::fiat::FiatInfoProvider;
use crate::providers::info::{DefaultInfoProvider, InfoProvider};
use crate::utils::context::Context;
use crate::utils::errors::{ApiResult};
use futures::{stream, StreamExt};
use std::str::FromStr;
use std::ops::{Div, Mul};
use num_traits::cast::FromPrimitive;
use num_traits::Pow;

pub async fn balances(
    context: &Context<'_>,
    chain_id: &str,
    safe_address: &str,
    fiat: &str,
    trusted: bool,
    exclude_spam: bool,
) -> ApiResult<Balances> {
    let info_provider = DefaultInfoProvider::new(chain_id, &context);
    let fiat_info_provider = FiatInfoProvider::new(&context);
    let url = core_uri!(
        info_provider,
        "/v1/safes/{}/balances/usd/?trusted={}&exclude_spam={}",
        safe_address,
        trusted,
        exclude_spam
    )?;

    let body = RequestCached::new(url)
        .cache_duration(balances_cache_duration())
        .request_timeout(balances_request_timeout())
        .execute(context.client(), context.cache())
        .await?;
    let backend_balances: Vec<BalanceDto> = serde_json::from_str(&body)?;

    let usd_to_fiat = fiat_info_provider
        .exchange_usd_to(fiat)
        .await
        .unwrap_or(0.0);

    let native_currency: NativeCurrency = info_provider.chain_info().await?.native_currency;

    let mut total_fiat = 0.0;

    let mut token_pairs: Vec<(String, String)> = vec![];
    for balance in &backend_balances {
        match &balance.token_address {
            None => {}
            Some(token_address) => {
                let url_result = core_uri!(info_provider, "/v1/tokens/{}/prices/usd/", token_address);
                match url_result {
                    Ok(url) => { token_pairs.push((token_address.to_owned(), url)) }
                    _ => {}
                }
            }
        }
    }

    println!("{:?}", token_pairs);
    let token_prices = stream::iter(token_pairs)
        .map(|(token_address, url)| request_token_usd_rate(context, token_address, url))
        .buffer_unordered(5)
        .filter_map(|t| async move {
            match t {
                Ok(token_price) => { Some(token_price) }
                Err(_) => { None }
            }
        })
        .collect::<Vec<_>>()
        .await;

    println!("{:?}", token_prices);

    let service_balances: Vec<Balance> = backend_balances
        .into_iter()
        .map(|it| {
            let b_token_address: String = it.token_address.to_owned().unwrap_or("0x0000000000000000000000000000000000000000".to_string());
            let b_token_decimals = it.token.as_ref().and_then(|token| Some(token.decimals)).unwrap_or(native_currency.decimals);
            let token_price: Option<&TokenPrice> = token_prices.iter().find(|&token_price| token_price.address == b_token_address);
            let usd_price: f64 = token_price.and_then(|t| Some(t.fiat_price)).unwrap_or(f64::from(0));

            let x = f64::from_u64(b_token_decimals).unwrap_or(f64::from(1));
            let balance = it.to_balance(usd_price.mul(usd_to_fiat).mul(f64::from(10).pow(-x)), &native_currency);
            total_fiat += balance.fiat_balance.parse::<f64>().unwrap_or(0.0);
            balance
        })
        .collect();

    Ok(Balances {
        fiat_total: total_fiat.to_string(),
        items: service_balances,
    })
}

async fn request_token_usd_rate(
    context: &Context<'_>,
    token_address: String,
    endpoint: String,
) -> ApiResult<TokenPrice> {
    let body = RequestCached::new(endpoint.to_owned())
        .cache_duration(balances_cache_duration()) // TODO change values
        .request_timeout(balances_request_timeout())
        .execute(context.client(), context.cache())
        .await?;
    let response: TokenPriceCore = serde_json::from_str(&body)?;

    let fiat_price = f64::from_str(&response.fiat_price).unwrap_or(0.0);

    return Ok(
        TokenPrice {
            address: token_address.to_string(),
            fiat_code: response.fiat_code,
            fiat_price,
            timestamp: response.timestamp,
        }
    );
}

pub async fn fiat_codes(context: &Context<'_>) -> ApiResult<Vec<String>> {
    let info_provider = FiatInfoProvider::new(&context);
    let mut fiat_codes = info_provider.available_currency_codes().await?;

    let usd_index = fiat_codes.iter().position(|it| it.eq("USD")).unwrap();
    let eur_index = fiat_codes.iter().position(|it| it.eq("EUR")).unwrap();

    let usd_code = fiat_codes.swap_remove(usd_index);
    let eur_code = fiat_codes.swap_remove(eur_index);

    fiat_codes.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

    let mut output = vec![usd_code, eur_code];
    output.append(&mut fiat_codes);

    Ok(output)
}
