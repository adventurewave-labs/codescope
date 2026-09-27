namespace App.Data {
    public class Repo {
        public void Save() { Flush(); } // @eval Flush=Repo::Flush
        private void Flush() {}
    }
    public class Cache {
        public void Save() {}
        private void Flush() {}
    }
}
