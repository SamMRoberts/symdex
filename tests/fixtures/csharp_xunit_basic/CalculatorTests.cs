namespace Demo;

class Calculator
{
    public int Add(int left, int right) => left + right;
}

class CalculatorTests
{
    [Fact]
    public void AddsNumbers()
    {
        var calculator = new Calculator();
        calculator.Add(1, 2);
    }

    [Theory]
    [InlineData(2, 3)]
    public void AddsParameterizedNumbers(int left, int right)
    {
        var calculator = new Calculator();
        calculator.Add(left, right);
    }
}
