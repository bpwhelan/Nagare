export function shouldAutoEnhance(card, autoApprove) {
  return card?.source === 'pending' && (autoApprove || card.skip_confirmation === true);
}

export function needsConfirmation(card, autoApprove) {
  return card?.source === 'retry' || (card?.source === 'pending' && !shouldAutoEnhance(card, autoApprove));
}
