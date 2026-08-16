export const ESCAPE_STOP_CONFIRMATION_MS = 3_000

export interface EscapeStopArm {
  target: string
  expiresAt: number
}

export type EscapeStopPress =
  | { type: 'arm'; arm: EscapeStopArm }
  | { type: 'stop' }

export function pressEscapeStop(
  current: EscapeStopArm | null,
  target: string,
  now: number,
): EscapeStopPress {
  if (isEscapeStopArmed(current, target, now)) return { type: 'stop' }
  return {
    type: 'arm',
    arm: {
      target,
      expiresAt: now + ESCAPE_STOP_CONFIRMATION_MS,
    },
  }
}

export function isEscapeStopArmed(
  current: EscapeStopArm | null,
  target: string,
  now: number,
): boolean {
  return Boolean(current && current.target === target && now < current.expiresAt)
}

export function sameEscapeStopArm(
  left: EscapeStopArm | null,
  right: EscapeStopArm,
): boolean {
  return Boolean(left && left.target === right.target && left.expiresAt === right.expiresAt)
}
