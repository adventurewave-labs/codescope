using System;
using App.Data;

namespace App.Core {
    public class OrderService {
        private void Validate() {}
        public void Place(Repo repo) {
            repo.Save(); // @eval Save=Repo::Save
            Validate(); // @eval Validate=OrderService::Validate
            Console.WriteLine("x"); // @eval WriteLine=-
            var o = new Order(); // @eval Order=Order::Order
        }
    }
    public class Order { public Order() {} }
}
