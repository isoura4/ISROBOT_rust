//! Commandes simples : /ping, /ping_bot, /coinflip.

use poise::CreateReply;
use rand::Rng;

use crate::bot::{Context, Error, colors, embed, locale};
use crate::tr;

/// Répond avec pong !
#[poise::command(slash_command)]
pub async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say(tr!("ping.response", locale(ctx))).await?;
    Ok(())
}

/// Mesure le ping entre Discord et le bot.
#[poise::command(slash_command)]
pub async fn ping_bot(ctx: Context<'_>) -> Result<(), Error> {
    let latency = ctx.ping().await.as_secs_f64() * 1000.0;
    ctx.say(tr!("ping_bot.response", locale(ctx), latency = latency))
        .await?;
    Ok(())
}

/// Lance une pièce et répond avec le résultat !
#[poise::command(slash_command)]
pub async fn coinflip(ctx: Context<'_>) -> Result<(), Error> {
    let loc = locale(ctx);
    let heads = rand::thread_rng().gen_bool(0.5);
    let result = tr!(
        if heads {
            "coinflip.heads"
        } else {
            "coinflip.tails"
        },
        loc
    );

    let e = embed(tr!("coinflip.title", loc), colors::BLUE).description(tr!(
        "coinflip.result",
        loc,
        result = result
    ));
    ctx.send(CreateReply::default().embed(e)).await?;
    Ok(())
}
