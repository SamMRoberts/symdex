export function typed(input: string): string {
  return helper(input);
}

const helper = (value: string): string => value.trim();
