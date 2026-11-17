import { Gateway, MockGateway } from './gateway.js';

export class Checkout {
  private gateway: Gateway;

  constructor(gateway?: Gateway) {
    // Field gateway is declared Gateway; the only in-repo impl is MockGateway.
    this.gateway = gateway ?? new MockGateway();
  }

  submit(cents: number, card: string): string {
    const receipt = this.gateway.charge(cents, card); // @eval charge=MockGateway::charge@src/gateway.ts
    audit(receipt); // @eval audit=audit@src/checkout.ts
    return receipt + cents.toFixed(2); // @eval toFixed=-
  }

  cancel(receipt: string): boolean {
    return this.gateway.refund(receipt); // @eval refund=MockGateway::refund@src/gateway.ts
  }
}

function audit(receipt: string): void {
  if (receipt.startsWith('mock:')) { // @eval startsWith=-
    console.info('audited', receipt);
  }
}
