namespace Pricing;

// Single in-repo implementation of IRateCard.
public interface IRateCard
{
    decimal RateFor(string zone);
}

public class FlatRateCard : IRateCard
{
    public decimal RateFor(string zone)
    {
        var key = NormalizeZone(zone); // @eval NormalizeZone=FlatRateCard::NormalizeZone@src/Pricing.cs
        return key == "EU" ? 0.19m : 0.25m;
    }

    internal static string NormalizeZone(string zone)
    {
        return zone.Trim().ToUpperInvariant(); // @eval Trim=-
    }
}
