import { RemovalForm } from "./removal-form";

export default function Cancel() {
  return (
    <main className="panel">
      <a className="brand" href="https://openxet.com/">OpenXet</a>
      <p className="eyebrow">Hosted storage waitlist</p>
      <h1>Remove your request.</h1>
      <p>This removes the saved email address from the waitlist. Opening this page does not remove anything.</p>
      <RemovalForm />
      <p className="small"><a href="https://openxet.com/">Keep my request and return to OpenXet</a></p>
    </main>
  );
}
