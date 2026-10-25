export const PASSWORD_MIN_LENGTH = 6;

/**
 * Returns a translation key describing the first failed password rule, or
 * null when the value is acceptable.
 */
export function validatePassword(value) {
  if ([...value].length < PASSWORD_MIN_LENGTH) {
    return "vault.passwordTooShort";
  }
  if (value !== value.trim()) {
    return "vault.passwordWhitespace";
  }
  if (/[^\x20-\x7E]/.test(value)) {
    return "vault.passwordInvalidChars";
  }
  if (!/[A-Za-z]/.test(value)) {
    return "vault.passwordNeedLetter";
  }
  if (!/[0-9]/.test(value)) {
    return "vault.passwordNeedDigit";
  }
  return null;
}