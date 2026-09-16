import type { ProviderModel, ProviderModelOption } from '@waku/client';

export interface ModelTraitSelection {
  reasoningEffort: string | null;
  serviceTier: string | null;
  contextWindow: string | null;
}

export function modelHasConfigurableTraits(model: ProviderModel): boolean {
  return Boolean(
    model.reasoning_efforts.length ||
      model.service_tiers.length ||
      model.context_windows.length,
  );
}

/** Resolve unset or stale persisted values to what the model picker should
 * present as selected. The underlying null is still preserved until the user
 * makes a choice, so provider defaults remain provider-owned. */
export function resolveModelTraitSelection(
  model: ProviderModel,
  selection: ModelTraitSelection,
): ModelTraitSelection {
  return {
    reasoningEffort: resolveOption(
      model.reasoning_efforts,
      selection.reasoningEffort,
      model.default_reasoning_effort ?? null,
    ),
    serviceTier: resolveServiceTier(model, selection.serviceTier),
    contextWindow: resolveOption(
      model.context_windows,
      selection.contextWindow,
      model.default_context_window ?? null,
    ),
  };
}

export function modelTraitSummary(
  model: ProviderModel,
  selection: ModelTraitSelection,
): string {
  const resolved = resolveModelTraitSelection(model, selection);
  const labels = [
    optionLabel(model.reasoning_efforts, resolved.reasoningEffort),
    model.service_tiers.length
      ? resolved.serviceTier === 'default'
        ? 'Standard'
        : optionLabel(model.service_tiers, resolved.serviceTier)
      : null,
    optionLabel(model.context_windows, resolved.contextWindow),
  ].filter((label): label is string => Boolean(label));
  return labels.join(' · ');
}

export function resolveServiceTier(model: ProviderModel, selected: string | null): string | null {
  if (!model.service_tiers.length) return null;
  if (selected === 'default' || hasOption(model.service_tiers, selected)) return selected;
  const fallback = model.default_service_tier ?? 'default';
  return fallback === 'default' || hasOption(model.service_tiers, fallback)
    ? fallback
    : 'default';
}

function resolveOption(
  options: ProviderModelOption[],
  selected: string | null,
  fallback: string | null,
): string | null {
  if (!options.length) return null;
  if (hasOption(options, selected)) return selected;
  if (hasOption(options, fallback)) return fallback;
  return options[0]?.id ?? null;
}

function hasOption(options: ProviderModelOption[], id: string | null): id is string {
  return Boolean(id && options.some((option) => option.id === id));
}

function optionLabel(options: ProviderModelOption[], id: string | null): string | null {
  if (!id) return null;
  return options.find((option) => option.id === id)?.label ?? id;
}
