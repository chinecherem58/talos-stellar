import { db } from "@/db";
import { tlsTalos, tlsCommerceJobs, tlsRevenues, tlsApprovals } from "@/db/schema";
import { and, eq, gte, sql } from "drizzle-orm";

// GET /api/talos/:id/exposure — Counterparty and financial exposure summary
export async function GET(
  _request: Request,
  { params }: { params: Promise<{ id: string }> }
) {
  const { id } = await params;

  try {
    const talos = await db
      .select({
        id: tlsTalos.id,
        name: tlsTalos.name,
      })
      .from(tlsTalos)
      .where(eq(tlsTalos.id, id))
      .limit(1)
      .then((r) => r[0] ?? null);

    if (!talos) {
      return Response.json({ error: "TALOS not found" }, { status: 404 });
    }

    const now = new Date();
    const thirtyDaysAgo = new Date(now.getTime() - 30 * 24 * 60 * 60 * 1000);
    const ninetyDaysAgo = new Date(now.getTime() - 90 * 24 * 60 * 60 * 1000);

    // ── Counterparty exposure via commerce jobs ──────────────────────
    // Aggregate spend and job counts per counterparty (requester) in 30-day window
    const counterpartyRows = await db
      .select({
        counterpartyId: tlsCommerceJobs.requesterTalosId,
        jobCount: sql<number>`count(*)::int`,
        totalAmount: sql<string>`coalesce(sum(${tlsCommerceJobs.amount}), '0')`,
        pendingCount: sql<number>`sum(case when ${tlsCommerceJobs.status} = 'pending' then 1 else 0 end)::int`,
        failedCount: sql<number>`sum(case when ${tlsCommerceJobs.status} = 'failed' then 1 else 0 end)::int`,
      })
      .from(tlsCommerceJobs)
      .where(
        and(
          eq(tlsCommerceJobs.talosId, id),
          gte(tlsCommerceJobs.createdAt, thirtyDaysAgo)
        )
      )
      .groupBy(tlsCommerceJobs.requesterTalosId);

    // ── Open (unfulfilled) job exposure ──────────────────────────────
    const [openJobsExposure] = await db
      .select({
        count: sql<number>`count(*)::int`,
        totalAmount: sql<string>`coalesce(sum(${tlsCommerceJobs.amount}), '0')`,
      })
      .from(tlsCommerceJobs)
      .where(
        and(
          eq(tlsCommerceJobs.talosId, id),
          sql`${tlsCommerceJobs.status} in ('pending', 'processing')`
        )
      );

    // ── Revenue exposure (dependency on single source) ───────────────
    const revenueBySource = await db
      .select({
        source: tlsRevenues.source,
        total: sql<string>`coalesce(sum(${tlsRevenues.amount}), '0')`,
        count: sql<number>`count(*)::int`,
      })
      .from(tlsRevenues)
      .where(
        and(
          eq(tlsRevenues.talosId, id),
          gte(tlsRevenues.createdAt, thirtyDaysAgo)
        )
      )
      .groupBy(tlsRevenues.source);

    const totalRevenue30d = revenueBySource.reduce(
      (acc, r) => acc + Number(r.total),
      0
    );

    // Concentration: largest single revenue source / total revenue
    const maxSourceRevenue = revenueBySource.reduce(
      (max, r) => Math.max(max, Number(r.total)),
      0
    );
    const revenueConcentrationPct =
      totalRevenue30d > 0
        ? Math.round((maxSourceRevenue / totalRevenue30d) * 10000) / 100
        : 0;

    // ── Pending approval exposure (uncommitted liabilities) ──────────
    const [pendingLiabilities] = await db
      .select({
        count: sql<number>`count(*)::int`,
        totalAmount: sql<string>`coalesce(sum(${tlsApprovals.amount}), '0')`,
      })
      .from(tlsApprovals)
      .where(
        and(
          eq(tlsApprovals.talosId, id),
          eq(tlsApprovals.status, "pending"),
          sql`${tlsApprovals.amount} is not null`
        )
      );

    // ── Approved spend 90-day for trend context ───────────────────────
    const [approvedSpend90d] = await db
      .select({
        total: sql<string>`coalesce(sum(${tlsApprovals.amount}), '0')`,
      })
      .from(tlsApprovals)
      .where(
        and(
          eq(tlsApprovals.talosId, id),
          eq(tlsApprovals.status, "approved"),
          sql`${tlsApprovals.amount} is not null`,
          gte(tlsApprovals.createdAt, ninetyDaysAgo)
        )
      );

    const totalCounterpartyExposure = counterpartyRows.reduce(
      (acc, r) => acc + Number(r.totalAmount),
      0
    );

    // Risk tier for overall exposure
    const exposureRatio =
      totalRevenue30d > 0
        ? (totalCounterpartyExposure / totalRevenue30d) * 100
        : 0;
    const riskTier =
      exposureRatio > 150
        ? "high"
        : exposureRatio > 75
        ? "medium"
        : "low";

    return Response.json({
      talosId: talos.id,
      talosName: talos.name,
      generatedAt: now.toISOString(),

      counterpartyExposure: {
        totalExposure30d: Math.round(totalCounterpartyExposure * 100) / 100,
        counterpartyCount: counterpartyRows.length,
        topCounterparties: counterpartyRows
          .sort((a, b) => Number(b.totalAmount) - Number(a.totalAmount))
          .slice(0, 5)
          .map((r) => ({
            counterpartyId: r.counterpartyId,
            jobCount: r.jobCount,
            totalAmount: Math.round(Number(r.totalAmount) * 100) / 100,
            pendingJobs: r.pendingCount,
            failedJobs: r.failedCount,
          })),
      },

      openJobExposure: {
        openJobs: openJobsExposure.count,
        totalOpenAmount: Math.round(Number(openJobsExposure.totalAmount) * 100) / 100,
      },

      revenueExposure: {
        totalRevenue30d: Math.round(totalRevenue30d * 100) / 100,
        revenueConcentrationPct,
        sourceCount: revenueBySource.length,
        sources: revenueBySource.map((r) => ({
          source: r.source,
          total: Math.round(Number(r.total) * 100) / 100,
          transactions: r.count,
        })),
      },

      liabilityExposure: {
        pendingApprovals: pendingLiabilities.count,
        pendingAmount: Math.round(Number(pendingLiabilities.totalAmount) * 100) / 100,
        approvedSpend90d: Math.round(Number(approvedSpend90d.total) * 100) / 100,
      },

      riskTier,
      exposureRatioPct: Math.round(exposureRatio * 100) / 100,
      riskMessage:
        riskTier === "high"
          ? "⚠ High exposure — counterparty spend significantly exceeds revenue"
          : riskTier === "medium"
          ? "⚠ Moderate exposure — monitor counterparty concentration"
          : "✓ Exposure within acceptable range",
    });
  } catch (err) {
    console.error("[exposure GET]", err);
    return Response.json({ error: "Internal server error" }, { status: 500 });
  }
}
