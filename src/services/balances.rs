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

    let service_balances: Vec<Balance> = backend_balances
        .into_iter()
        .map(|it| {
            let balance = it.to_balance(usd_to_fiat, &native_currency);
            total_fiat += balance.fiat_balance.parse::<f64>().unwrap_or(0.0);
            balance
        })
        .collect();

    let mut token_pairs: Vec<(String, String)> = vec![];
    for balance in &service_balances {
        let token_address = balance.token_info.address.to_owned();
        let url_result = core_uri!(info_provider, "/tokens/{}/prices/usd/", token_address);
        match url_result {
            Ok(url) => { token_pairs.push((token_address, url)) }
            _ => {}
        }
    }

    let token_prices = stream::iter(token_pairs)
        .map(|(token_address, url)| request_token_usd_rate(token_address, url))
        .buffer_unordered(5)
        .filter_map(|t| async move {
            match t {
                Ok(token_price) => { Some(token_price) }
                Err(_) => { None }
            }
        })
        .collect::<Vec<_>>()
        .await;

    Ok(Balances {
        fiat_total: total_fiat.to_string(),
        items: service_balances,
    })
}

pub async fn request_token_usd_rate(
    token_address: String,
    endpoint: String,
) -> ApiResult<TokenPrice> {
    let response: TokenPriceCore = reqwest::get(endpoint).await?.json::<TokenPriceCore>().await?;
    return Ok(
        TokenPrice {
            address: token_address.to_string(),
            fiat_code: response.fiat_code,
            fiat_price: response.fiat_price,
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
