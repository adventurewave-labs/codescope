using System.Collections.Generic;
using System.Linq;
using Pricing;

namespace Orders;

public class Invoice
{
    private readonly IRateCard _rates;

    public Invoice(IRateCard rates)
    {
        _rates = rates;
    }

    public decimal Total(string zone, List<decimal> lines)
    {
        var rate = _rates.RateFor(zone); // @eval RateFor=IRateCard::RateFor@src/Pricing.cs
        var subtotal = lines.Sum(line => line); // @eval Sum=-
        return ApplyTax(subtotal, rate); // @eval ApplyTax=Invoice::ApplyTax@src/Invoice.cs
    }

    internal static decimal ApplyTax(decimal amount, decimal rate)
    {
        return Math.Round(amount * (1 + rate), 2); // @eval Round=-
    }
}
