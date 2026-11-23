//! Petits utilitaires d'interface Discord : modales, réponses aux interactions.

use std::time::Duration;

use poise::serenity_prelude as serenity;

/// Champ de saisie d'une modale.
pub struct Input {
    pub id: &'static str,
    pub label: String,
    pub placeholder: Option<String>,
    pub paragraph: bool,
    pub required: bool,
    pub max_length: u16,
}

impl Input {
    pub fn short(id: &'static str, label: String) -> Self {
        Self {
            id,
            label,
            placeholder: None,
            paragraph: false,
            required: true,
            max_length: 100,
        }
    }
    pub fn paragraph(mut self) -> Self {
        self.paragraph = true;
        self
    }
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }
    pub fn placeholder(mut self, p: String) -> Self {
        self.placeholder = Some(p);
        self
    }
    pub fn max(mut self, n: u16) -> Self {
        self.max_length = n;
        self
    }
}

pub fn modal(
    custom_id: &str,
    title: &str,
    inputs: &[Input],
) -> serenity::CreateInteractionResponse {
    let rows = inputs
        .iter()
        .map(|i| {
            let style = if i.paragraph {
                serenity::InputTextStyle::Paragraph
            } else {
                serenity::InputTextStyle::Short
            };
            let mut t = serenity::CreateInputText::new(style, &i.label, i.id)
                .required(i.required)
                .max_length(i.max_length);
            if let Some(p) = &i.placeholder {
                t = t.placeholder(p);
            }
            serenity::CreateActionRow::InputText(t)
        })
        .collect();
    // Discord limite le titre d'une modale à 45 caractères.
    let title: String = title.chars().take(45).collect();
    serenity::CreateInteractionResponse::Modal(
        serenity::CreateModal::new(custom_id, title).components(rows),
    )
}

/// Attend la soumission d'une modale (5 minutes max).
pub async fn wait_modal(
    ctx: &serenity::Context,
    custom_id: String,
) -> Option<serenity::ModalInteraction> {
    serenity::ModalInteractionCollector::new(ctx)
        .custom_ids(vec![custom_id])
        .timeout(Duration::from_secs(300))
        .await
}

/// Valeur d'un champ d'une modale soumise.
pub fn modal_value(mi: &serenity::ModalInteraction, id: &str) -> String {
    mi.data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|c| match c {
            serenity::ActionRowComponent::InputText(t) if t.custom_id == id => t.value.clone(),
            _ => None,
        })
        .unwrap_or_default()
}

/// Message éphémère de suivi après un `defer_ephemeral`.
pub fn followup(content: impl Into<String>) -> serenity::CreateInteractionResponseFollowup {
    serenity::CreateInteractionResponseFollowup::new()
        .content(content)
        .ephemeral(true)
}

/// Réponse éphémère immédiate à une interaction.
pub fn ephemeral(content: impl Into<String>) -> serenity::CreateInteractionResponse {
    serenity::CreateInteractionResponse::Message(
        serenity::CreateInteractionResponseMessage::new()
            .content(content)
            .ephemeral(true),
    )
}

/// Identifiant unique pour une modale.
pub fn unique_id(prefix: &str) -> String {
    format!("{prefix}:{}", rand::random::<u64>())
}
