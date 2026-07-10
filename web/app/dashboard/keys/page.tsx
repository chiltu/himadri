"use client"

import { AuthGuard } from "@/components/auth-guard"
import { useEffect, useState, useCallback } from "react"
import { api, type ApiKey, type CreateApiKeyRequest, type UpdateApiKeyRequest } from "@/lib/api"
import { AppSidebar } from "@/components/app-sidebar"
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb"
import { Separator } from "@/components/ui/separator"
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar"
import { Button } from "@/components/ui/button"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

interface KeyFormState {
  name: string
  scopes: string
  orgId: string
  teamId: string
  userId: string
  models: string
  rpsLimit: string
  burstSize: string
  maxTokensPerRequest: string
  maxTokensPerDay: string
  maxTokensPerMonth: string
  costLimitPerDay: string
  costLimitPerMonth: string
}

const emptyForm: KeyFormState = {
  name: "",
  scopes: "api",
  orgId: "",
  teamId: "",
  userId: "",
  models: "",
  rpsLimit: "",
  burstSize: "",
  maxTokensPerRequest: "",
  maxTokensPerDay: "",
  maxTokensPerMonth: "",
  costLimitPerDay: "",
  costLimitPerMonth: "",
}

function formFromKey(key: ApiKey): KeyFormState {
  return {
    name: key.name,
    scopes: key.scopes.join(", "),
    orgId: key.org_id ?? "",
    teamId: key.team_id ?? "",
    userId: key.user_id ?? "",
    models: key.models?.join(", ") ?? "",
    rpsLimit: key.rate_limit_override?.requests_per_second?.toString() ?? "",
    burstSize: key.rate_limit_override?.burst_size?.toString() ?? "",
    maxTokensPerRequest: key.token_budget?.max_tokens_per_request?.toString() ?? "",
    maxTokensPerDay: key.token_budget?.max_tokens_per_day?.toString() ?? "",
    maxTokensPerMonth: key.token_budget?.max_tokens_per_month?.toString() ?? "",
    costLimitPerDay: key.token_budget?.cost_limit_per_day?.toString() ?? "",
    costLimitPerMonth: key.token_budget?.cost_limit_per_month?.toString() ?? "",
  }
}

function num(v: string): number | undefined {
  if (v.trim() === "") return undefined
  const n = Number(v)
  return Number.isNaN(n) ? undefined : n
}

function list(v: string): string[] | undefined {
  const items = v.split(",").map((s) => s.trim()).filter(Boolean)
  return items.length ? items : undefined
}

function buildRateLimitOverride(form: KeyFormState) {
  const rps = num(form.rpsLimit)
  const burst = num(form.burstSize)
  if (rps === undefined && burst === undefined) return undefined
  return { requests_per_second: rps, burst_size: burst }
}

function buildTokenBudget(form: KeyFormState) {
  const maxReq = num(form.maxTokensPerRequest)
  const maxDay = num(form.maxTokensPerDay)
  const maxMonth = num(form.maxTokensPerMonth)
  const costDay = num(form.costLimitPerDay)
  const costMonth = num(form.costLimitPerMonth)
  if ([maxReq, maxDay, maxMonth, costDay, costMonth].every((v) => v === undefined)) return undefined
  return {
    max_tokens_per_request: maxReq,
    max_tokens_per_day: maxDay,
    max_tokens_per_month: maxMonth,
    cost_limit_per_day: costDay,
    cost_limit_per_month: costMonth,
  }
}

function KeyFormFields({
  form,
  onChange,
}: {
  form: KeyFormState
  onChange: (form: KeyFormState) => void
}) {
  const set = <K extends keyof KeyFormState>(key: K, value: KeyFormState[K]) =>
    onChange({ ...form, [key]: value })

  return (
    <Tabs defaultValue="basic">
      <TabsList>
        <TabsTrigger value="basic">Basic</TabsTrigger>
        <TabsTrigger value="budgets">Budgets &amp; Limits</TabsTrigger>
      </TabsList>
      <TabsContent value="basic" className="space-y-4 pt-2">
        <div>
          <Label>Name</Label>
          <Input value={form.name} onChange={(e) => set("name", e.target.value)} placeholder="e.g. production-key" />
        </div>
        <div>
          <Label>Scopes (comma-separated)</Label>
          <Input value={form.scopes} onChange={(e) => set("scopes", e.target.value)} placeholder="api, admin" />
        </div>
        <div>
          <Label>Org ID</Label>
          <Input value={form.orgId} onChange={(e) => set("orgId", e.target.value)} placeholder="optional" />
        </div>
        <div>
          <Label>Team ID</Label>
          <Input value={form.teamId} onChange={(e) => set("teamId", e.target.value)} placeholder="optional" />
        </div>
        <div>
          <Label>User ID</Label>
          <Input value={form.userId} onChange={(e) => set("userId", e.target.value)} placeholder="optional" />
        </div>
        <div>
          <Label>Allowed models (comma-separated)</Label>
          <Input value={form.models} onChange={(e) => set("models", e.target.value)} placeholder="leave empty to allow all" />
        </div>
      </TabsContent>
      <TabsContent value="budgets" className="space-y-4 pt-2">
        <div className="grid grid-cols-2 gap-4">
          <div>
            <Label>Requests/sec limit</Label>
            <Input type="number" value={form.rpsLimit} onChange={(e) => set("rpsLimit", e.target.value)} placeholder="unlimited" />
          </div>
          <div>
            <Label>Burst size</Label>
            <Input type="number" value={form.burstSize} onChange={(e) => set("burstSize", e.target.value)} placeholder="unlimited" />
          </div>
          <div>
            <Label>Max tokens / request</Label>
            <Input type="number" value={form.maxTokensPerRequest} onChange={(e) => set("maxTokensPerRequest", e.target.value)} placeholder="unlimited" />
          </div>
          <div>
            <Label>Max tokens / day</Label>
            <Input type="number" value={form.maxTokensPerDay} onChange={(e) => set("maxTokensPerDay", e.target.value)} placeholder="unlimited" />
          </div>
          <div>
            <Label>Max tokens / month</Label>
            <Input type="number" value={form.maxTokensPerMonth} onChange={(e) => set("maxTokensPerMonth", e.target.value)} placeholder="unlimited" />
          </div>
          <div>
            <Label>Cost limit / day ($)</Label>
            <Input type="number" value={form.costLimitPerDay} onChange={(e) => set("costLimitPerDay", e.target.value)} placeholder="unlimited" />
          </div>
          <div>
            <Label>Cost limit / month ($)</Label>
            <Input type="number" value={form.costLimitPerMonth} onChange={(e) => set("costLimitPerMonth", e.target.value)} placeholder="unlimited" />
          </div>
        </div>
      </TabsContent>
    </Tabs>
  )
}

export default function KeysPage() {
  const [keys, setKeys] = useState<ApiKey[]>([])
  const [error, setError] = useState<string | null>(null)
  const [createOpen, setCreateOpen] = useState(false)
  const [createForm, setCreateForm] = useState<KeyFormState>(emptyForm)
  const [createdKey, setCreatedKey] = useState<ApiKey | null>(null)
  const [editKey, setEditKey] = useState<ApiKey | null>(null)
  const [editForm, setEditForm] = useState<KeyFormState>(emptyForm)

  const loadKeys = useCallback(() => {
    api.listKeys().then(setKeys).catch((e) => setError(e.message))
  }, [])

  useEffect(() => {
    loadKeys()
  }, [loadKeys])

  const handleCreate = async () => {
    try {
      const req: CreateApiKeyRequest = {
        name: createForm.name,
        scopes: list(createForm.scopes) ?? [],
        org_id: createForm.orgId || undefined,
        team_id: createForm.teamId || undefined,
        user_id: createForm.userId || undefined,
        models: list(createForm.models),
        rate_limit_override: buildRateLimitOverride(createForm),
        token_budget: buildTokenBudget(createForm),
      }
      const key = await api.createKey(req)
      setCreatedKey(key)
      setCreateOpen(false)
      setCreateForm(emptyForm)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to create key")
    }
  }

  const openEdit = (key: ApiKey) => {
    setEditKey(key)
    setEditForm(formFromKey(key))
  }

  const handleSaveEdit = async () => {
    if (!editKey) return
    try {
      const req: UpdateApiKeyRequest = {
        name: editForm.name,
        scopes: list(editForm.scopes) ?? [],
        org_id: editForm.orgId || null,
        team_id: editForm.teamId || null,
        user_id: editForm.userId || null,
        models: list(editForm.models) ?? null,
        rate_limit_override: buildRateLimitOverride(editForm) ?? null,
        token_budget: buildTokenBudget(editForm) ?? null,
      }
      await api.updateKey(editKey.id, req)
      setEditKey(null)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to update key")
    }
  }

  const handleRevoke = async (id: string) => {
    if (!confirm("Revoke this key? It will stop working immediately.")) return
    try {
      await api.revokeKey(id)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to revoke key")
    }
  }

  const handleRotate = async (id: string) => {
    if (!confirm("Rotate this key? The old key will be invalidated.")) return
    try {
      const newKey = await api.rotateKey(id)
      setCreatedKey(newKey)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to rotate key")
    }
  }

  const handleDelete = async (id: string) => {
    if (!confirm("Permanently delete this key?")) return
    try {
      await api.deleteKey(id)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to delete key")
    }
  }

  const handleToggle = async (key: ApiKey) => {
    try {
      await api.updateKey(key.id, { enabled: !key.enabled })
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to update key")
    }
  }

  return (
    <AuthGuard>
    <SidebarProvider>
      <AppSidebar />
      <SidebarInset>
        <header className="flex h-16 shrink-0 items-center gap-2 transition-[width,height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-12">
          <div className="flex items-center gap-2 px-4">
            <SidebarTrigger className="-ml-1" />
            <Separator orientation="vertical" className="mr-2 data-vertical:h-4 data-vertical:self-auto" />
            <Breadcrumb>
              <BreadcrumbList>
                <BreadcrumbItem className="hidden md:block">
                  <BreadcrumbLink href="#">API Gateway</BreadcrumbLink>
                </BreadcrumbItem>
                <BreadcrumbSeparator className="hidden md:block" />
                <BreadcrumbItem>
                  <BreadcrumbPage>All Keys</BreadcrumbPage>
                </BreadcrumbItem>
              </BreadcrumbList>
            </Breadcrumb>
          </div>
        </header>
        <div className="flex flex-1 flex-col gap-4 p-4 pt-0">
          {error && (
            <div className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
              {error}
              <Button variant="ghost" size="sm" className="ml-2" onClick={() => setError(null)}>Dismiss</Button>
            </div>
          )}

          {createdKey && (
            <div className="rounded-md border border-green-500 bg-green-500/10 p-4">
              <p className="text-sm font-medium text-green-600 mb-2">Key Created — Copy it now, it won&apos;t be shown again.</p>
              <code className="block rounded bg-muted p-3 font-mono text-sm break-all">{createdKey.key}</code>
              <Button variant="outline" size="sm" className="mt-2" onClick={() => navigator.clipboard.writeText(createdKey.key)}>
                Copy to Clipboard
              </Button>
            </div>
          )}

          <div className="flex items-center justify-between">
            <h1 className="text-lg font-semibold">All Keys</h1>
            <Dialog open={createOpen} onOpenChange={setCreateOpen}>
              <DialogTrigger asChild>
                <Button>Create Key</Button>
              </DialogTrigger>
              <DialogContent>
                <DialogHeader>
                  <DialogTitle>Create API Key</DialogTitle>
                </DialogHeader>
                <div className="space-y-4">
                  <KeyFormFields form={createForm} onChange={setCreateForm} />
                  <Button onClick={handleCreate} className="w-full">Create</Button>
                </div>
              </DialogContent>
            </Dialog>
          </div>

          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Key</TableHead>
                <TableHead>Scopes</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Created</TableHead>
                <TableHead className="text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {keys.map((k) => (
                <TableRow key={k.id}>
                  <TableCell className="font-medium">{k.name}</TableCell>
                  <TableCell className="font-mono text-sm">{k.key.slice(0, 8)}...{k.key.slice(-4)}</TableCell>
                  <TableCell>
                    <div className="flex gap-1">
                      {k.scopes.map((s) => (
                        <Badge key={s} variant="secondary">{s}</Badge>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell>
                    <Badge variant={k.enabled ? "default" : "destructive"}>
                      {k.enabled ? "Active" : "Disabled"}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-sm">{new Date(k.created_at).toLocaleDateString()}</TableCell>
                  <TableCell className="text-right">
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="ghost" size="sm">...</Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <DropdownMenuItem onClick={() => openEdit(k)}>Edit</DropdownMenuItem>
                        <DropdownMenuItem onClick={() => handleToggle(k)}>
                          {k.enabled ? "Disable" : "Enable"}
                        </DropdownMenuItem>
                        <DropdownMenuItem onClick={() => handleRotate(k.id)}>Rotate Key</DropdownMenuItem>
                        <DropdownMenuItem onClick={() => handleRevoke(k.id)}>Revoke</DropdownMenuItem>
                        <DropdownMenuItem onClick={() => handleDelete(k.id)} className="text-destructive">Delete</DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </TableCell>
                </TableRow>
              ))}
              {keys.length === 0 && (
                <TableRow>
                  <TableCell colSpan={6} className="text-center text-muted-foreground py-8">No API keys yet</TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      </SidebarInset>
    </SidebarProvider>

    <Dialog open={editKey !== null} onOpenChange={(open) => !open && setEditKey(null)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Edit API Key</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          <KeyFormFields form={editForm} onChange={setEditForm} />
          <Button onClick={handleSaveEdit} className="w-full">Save Changes</Button>
        </div>
      </DialogContent>
    </Dialog>
    </AuthGuard>
  )
}
