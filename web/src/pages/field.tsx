import { Flex, Text, TextField } from '@radix-ui/themes';

interface FieldProps {
  name: string;
  label: string;
  type: 'text' | 'password';
  autoComplete: string;
  hint?: string;
  invalid?: boolean;
  autoFocus?: boolean;
}

export function Field({
  name,
  label,
  type,
  autoComplete,
  hint,
  invalid = false,
  autoFocus = false,
}: FieldProps) {
  const hintId = `${name}-hint`;
  return (
    <Flex direction="column" gap="1">
      <Text as="label" htmlFor={name} size="2" weight="medium">
        {label}
      </Text>
      <TextField.Root
        id={name}
        name={name}
        type={type}
        size="3"
        required
        autoComplete={autoComplete}
        autoFocus={autoFocus}
        maxLength={1024}
        aria-invalid={invalid}
        aria-describedby={hint === undefined ? undefined : hintId}
      />
      {hint === undefined ? null : (
        <Text id={hintId} size="2" color="gray">
          {hint}
        </Text>
      )}
    </Flex>
  );
}
