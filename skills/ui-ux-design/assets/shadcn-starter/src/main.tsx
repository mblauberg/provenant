import { Button } from "@/components/ui/button"
import { createRoot } from "react-dom/client"
import { useState } from "react"
import "./index.css"

function App() {
  const [name, setName] = useState("")
  const [submitted, setSubmitted] = useState("")
  return <main className="mx-auto max-w-lg p-8">
    <h1 className="text-2xl font-semibold">Welcome</h1>
    <form className="mt-6 grid gap-3" onSubmit={(event) => { event.preventDefault(); setSubmitted(name) }}>
      <label htmlFor="name">Name</label>
      <input id="name" required value={name} onChange={(event) => setName(event.target.value)} className="rounded border border-border bg-background p-2" />
      <Button type="submit">Continue</Button>
    </form>
    {submitted && <p role="status" className="mt-4">Hello, {submitted}.</p>}
  </main>
}

createRoot(document.getElementById("root")!).render(<App />)
