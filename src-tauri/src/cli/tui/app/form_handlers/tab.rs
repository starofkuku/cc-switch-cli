use super::*;

impl App {
    pub(super) fn handle_form_tab_key(&mut self, key: KeyEvent, data: &UiData) -> bool {
        let is_backtab = matches!(key.code, KeyCode::BackTab)
            || (matches!(key.code, KeyCode::Tab) && key.modifiers.contains(KeyModifiers::SHIFT));
        let is_tab = matches!(key.code, KeyCode::Tab) && !is_backtab;
        if !is_tab && !is_backtab {
            return false;
        }

        if self.form.as_ref().is_some_and(FormState::is_editing)
            && !matches!(
                self.form.as_ref(),
                Some(FormState::PromptMeta(prompt))
                    if matches!(prompt.focus, FormFocus::Content)
                        && prompt.text_edit.is_none()
            )
        {
            return self.handle_inline_edit_tab(is_backtab, data);
        }

        let Some(form) = self.form.as_mut() else {
            return false;
        };

        match form {
            FormState::ProviderAdd(provider) => {
                if matches!(
                    provider.page,
                    form::ProviderFormPage::ClaudeQuickConfig
                        | form::ProviderFormPage::CodexQuickConfig
                ) {
                    if is_backtab {
                        return false;
                    }
                    provider.focus = FormFocus::Fields;
                    return true;
                }
                if matches!(provider.page, form::ProviderFormPage::CodexLocalRouting) {
                    if is_backtab {
                        return false;
                    }
                    provider.focus = FormFocus::Fields;
                    return true;
                }
                if matches!(provider.page, form::ProviderFormPage::LocalProxySettings) {
                    if is_backtab {
                        return false;
                    }
                    provider.focus = FormFocus::Fields;
                    return true;
                }
                if matches!(provider.page, form::ProviderFormPage::CodexModelCatalog) {
                    if is_backtab {
                        return false;
                    }
                    provider.focus = FormFocus::Fields;
                    return true;
                }
                if matches!(provider.page, form::ProviderFormPage::UsageQuery) {
                    if is_backtab {
                        return false;
                    }
                    if !provider.usage_query_extractor_available() {
                        provider.focus = FormFocus::Fields;
                        return true;
                    }
                    provider.focus = match provider.focus {
                        FormFocus::Fields => FormFocus::JsonPreview,
                        FormFocus::JsonPreview => FormFocus::Content,
                        FormFocus::Content => FormFocus::Fields,
                        FormFocus::Templates => FormFocus::Fields,
                    };
                    return true;
                }
                if is_backtab {
                    return false;
                }
                if matches!(provider.app_type, AppType::Codex) {
                    match (
                        &provider.mode,
                        provider.focus,
                        provider.codex_preview_section,
                    ) {
                        (FormMode::Add, FormFocus::Templates, _) => {
                            provider.focus = FormFocus::Fields;
                        }
                        (FormMode::Add, FormFocus::Fields, _) => {
                            provider.focus = FormFocus::JsonPreview;
                            provider.codex_preview_section = form::CodexPreviewSection::Auth;
                        }
                        (
                            FormMode::Add,
                            FormFocus::JsonPreview,
                            form::CodexPreviewSection::Auth,
                        ) => {
                            provider.focus = FormFocus::JsonPreview;
                            provider.codex_preview_section = form::CodexPreviewSection::Config;
                        }
                        (
                            FormMode::Add,
                            FormFocus::JsonPreview,
                            form::CodexPreviewSection::Config,
                        ) => {
                            provider.focus = FormFocus::Templates;
                        }
                        (FormMode::Edit { .. }, FormFocus::Fields, _) => {
                            provider.focus = FormFocus::JsonPreview;
                            provider.codex_preview_section = form::CodexPreviewSection::Auth;
                        }
                        (
                            FormMode::Edit { .. },
                            FormFocus::JsonPreview,
                            form::CodexPreviewSection::Auth,
                        ) => {
                            provider.focus = FormFocus::JsonPreview;
                            provider.codex_preview_section = form::CodexPreviewSection::Config;
                        }
                        (
                            FormMode::Edit { .. },
                            FormFocus::JsonPreview,
                            form::CodexPreviewSection::Config,
                        ) => {
                            provider.focus = FormFocus::Fields;
                        }
                        (FormMode::Edit { .. }, FormFocus::Templates, _) => {
                            provider.focus = FormFocus::Fields;
                        }
                        (_, FormFocus::Content, _) => {
                            provider.focus = FormFocus::Fields;
                        }
                    }
                } else {
                    provider.focus = match (&provider.mode, provider.focus) {
                        (FormMode::Add, FormFocus::Templates) => FormFocus::Fields,
                        (FormMode::Add, FormFocus::Fields) => FormFocus::JsonPreview,
                        (FormMode::Add, FormFocus::JsonPreview) => FormFocus::Templates,
                        (FormMode::Add, FormFocus::Content) => FormFocus::Fields,
                        (FormMode::Edit { .. }, FormFocus::Fields) => FormFocus::JsonPreview,
                        (FormMode::Edit { .. }, FormFocus::JsonPreview) => FormFocus::Fields,
                        (FormMode::Edit { .. }, FormFocus::Templates) => FormFocus::Fields,
                        (FormMode::Edit { .. }, FormFocus::Content) => FormFocus::Fields,
                    };
                }
            }
            FormState::McpAdd(mcp) => {
                if is_backtab {
                    return false;
                }
                mcp.focus = match (&mcp.mode, mcp.focus) {
                    (FormMode::Add, FormFocus::Templates) => FormFocus::Fields,
                    (FormMode::Add, FormFocus::Fields) => FormFocus::JsonPreview,
                    (FormMode::Add, FormFocus::JsonPreview) => FormFocus::Templates,
                    (FormMode::Add, FormFocus::Content) => FormFocus::Fields,
                    (FormMode::Edit { .. }, FormFocus::Fields) => FormFocus::JsonPreview,
                    (FormMode::Edit { .. }, FormFocus::JsonPreview) => FormFocus::Fields,
                    (FormMode::Edit { .. }, FormFocus::Templates) => FormFocus::Fields,
                    (FormMode::Edit { .. }, FormFocus::Content) => FormFocus::Fields,
                };
            }
            FormState::PromptMeta(prompt) => {
                if is_backtab {
                    prompt.focus = FormFocus::Fields;
                    return true;
                }
                if is_tab && matches!(prompt.focus, FormFocus::Content) {
                    return false;
                }
                prompt.focus = match prompt.focus {
                    FormFocus::Fields => FormFocus::Content,
                    FormFocus::Content => FormFocus::Fields,
                    FormFocus::Templates | FormFocus::JsonPreview => FormFocus::Fields,
                };
            }
        }

        true
    }

    fn handle_inline_edit_tab(&mut self, backwards: bool, data: &UiData) -> bool {
        let commit = self.commit_active_text_edit(data);
        let Some(commit) = commit else {
            return false;
        };
        if !commit.valid {
            return true;
        }

        match commit.field {
            ActiveTextField::Provider(form::ProviderTextField::Main(field)) => {
                let Some(FormState::ProviderAdd(provider)) = self.form.as_mut() else {
                    return true;
                };
                let fields = provider.fields();
                let current = fields
                    .iter()
                    .position(|candidate| *candidate == field)
                    .unwrap_or(provider.field_idx.min(fields.len().saturating_sub(1)));
                if let Some(index) = adjacent_index(fields.len(), current, backwards, |index| {
                    provider.can_edit_main_field(fields[index])
                }) {
                    provider.field_idx = index;
                    provider.begin_main_text_edit(fields[index]);
                }
            }
            ActiveTextField::Provider(form::ProviderTextField::UsageQuery(field)) => {
                let Some(FormState::ProviderAdd(provider)) = self.form.as_mut() else {
                    return true;
                };
                let fields = provider.usage_query_table_fields();
                let current = fields
                    .iter()
                    .position(|candidate| *candidate == field)
                    .unwrap_or(
                        provider
                            .usage_query_field_idx
                            .min(fields.len().saturating_sub(1)),
                    );
                if let Some(index) = adjacent_index(fields.len(), current, backwards, |index| {
                    provider.can_edit_usage_query_field(fields[index])
                }) {
                    provider.usage_query_field_idx = index;
                    provider.begin_usage_query_text_edit(fields[index]);
                }
            }
            ActiveTextField::Mcp(field) => {
                let Some(FormState::McpAdd(mcp)) = self.form.as_mut() else {
                    return true;
                };
                let fields = mcp.fields();
                let current = fields
                    .iter()
                    .position(|candidate| *candidate == field)
                    .unwrap_or(mcp.field_idx.min(fields.len().saturating_sub(1)));
                if let Some(index) = adjacent_index(fields.len(), current, backwards, |index| {
                    mcp.can_edit_field(fields[index])
                }) {
                    mcp.field_idx = index;
                    mcp.begin_text_edit(fields[index]);
                }
            }
            ActiveTextField::Prompt(field) => {
                let Some(FormState::PromptMeta(prompt)) = self.form.as_mut() else {
                    return true;
                };
                let fields = prompt.fields();
                let current = fields
                    .iter()
                    .position(|candidate| *candidate == field)
                    .unwrap_or(prompt.field_idx.min(fields.len().saturating_sub(1)));
                if let Some(index) = adjacent_index(fields.len(), current, backwards, |_| true) {
                    prompt.field_idx = index;
                    prompt.begin_text_edit(fields[index]);
                }
            }
        }

        true
    }
}

fn adjacent_index(
    len: usize,
    current: usize,
    backwards: bool,
    mut editable: impl FnMut(usize) -> bool,
) -> Option<usize> {
    if backwards {
        (0..current).rev().find(|index| editable(*index))
    } else {
        ((current + 1)..len).find(|index| editable(*index))
    }
}
