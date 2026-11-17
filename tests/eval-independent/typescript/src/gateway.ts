// Payment gateway. Exactly ONE in-repo implementation of Gateway exists,
// so a Gateway-typed receiver's charge() has a single executable target.
export interface Gateway {
  charge(cents: number, card: string): string;
  refund(id: string): boolean;
}

export class MockGateway implements Gateway {
  charge(cents: number, card: string): string {
    return `mock:${maskCard(card)}:${cents}`; // @eval maskCard=maskCard@src/gateway.ts
  }

  refund(id: string): boolean {
    return id.length > 0;
  }
}

export function maskCard(card: string): string {
  return card.slice(-4).padStart(card.length, '*');
}
