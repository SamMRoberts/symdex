namespace Demo;

class Calculator
{
    public int Add(int left, int right) => left + right;
}

class CalculatorTests
{
    [Test]
    public void AddsNumbers()
    {
        var calculator = new Calculator();
        calculator.Add(1, 2);
    }

    [TestCase(2, 3)]
    public void AddsParameterizedNumbers(int left, int right)
    {
        var calculator = new Calculator();
        calculator.Add(left, right);
    }
}
