package io.github.kclejeune.sparkles.jena.engine

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.SparklesFallback
import io.github.kclejeune.sparkles.jena.SparklesInternalException
import io.github.kclejeune.sparkles.jena.internal.ffi.ErrorKind
import io.github.kclejeune.sparkles.jena.internal.ffi.FfiException
import io.github.kclejeune.sparkles.jena.internal.ffi.InternalException
import io.github.kclejeune.sparkles.jena.internal.mapError
import org.apache.jena.atlas.lib.Sink
import org.apache.jena.query.QueryExecException
import org.apache.jena.query.TxnType
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.engine.binding.Binding
import org.apache.jena.sparql.modify.UpdateEngine
import org.apache.jena.sparql.modify.UpdateEngineFactory
import org.apache.jena.sparql.modify.UpdateEngineRegistry
import org.apache.jena.sparql.modify.UpdateEngineWorker
import org.apache.jena.sparql.modify.UpdateSink
import org.apache.jena.sparql.modify.request.QuadDataAccSink
import org.apache.jena.sparql.modify.request.UpdateDataDelete
import org.apache.jena.sparql.modify.request.UpdateDataInsert
import org.apache.jena.sparql.util.Context
import org.apache.jena.update.Update
import org.apache.jena.update.UpdateRequest
import org.slf4j.LoggerFactory

/**
 * The update engine (P04 §3.6). `INSERT DATA` and `DELETE DATA` go to the write
 * transaction as quad batches, consecutive other operations run in Sparkles as one request
 * text, and an operation that needs Java runs in ARQ's worker over the same transaction,
 * in order. A request outside a transaction is one commit.
 */
public object UpdateEngineSparkles {
    private val log = LoggerFactory.getLogger(UpdateEngineSparkles::class.java)

    /** The factory registered with Jena's `UpdateEngineRegistry`. */
    @JvmField
    public val factory: UpdateEngineFactory = java.lang.reflect.Proxy.newProxyInstance(
        UpdateEngineFactory::class.java.classLoader, arrayOf(UpdateEngineFactory::class.java),
    ) { proxy, method, args ->
        when (method.name) {
            "accept" -> args!![0] is DatasetGraphSparkles && fallbackMode(args[0] as DatasetGraphSparkles, args[1] as Context?) != SparklesFallback.ALWAYS
            // Jena 6 removes initial bindings from its update factory. Keep one artifact
            // by adapting this tiny factory seam instead of linking either constructor.
            "create" -> Engine(args!![0] as DatasetGraphSparkles, if (args.size == 3) args[1] as Binding? else null, args.last() as Context?)
            "equals" -> proxy === args!![0]
            "hashCode" -> System.identityHashCode(proxy)
            "toString" -> "Sparkles update engine factory"
            else -> throw UnsupportedOperationException(method.name)
        }
    } as UpdateEngineFactory

    @JvmStatic
    @Synchronized
    public fun register() {
        if (!UpdateEngineRegistry.get().contains(factory)) UpdateEngineRegistry.addFactory(factory)
    }

    @JvmStatic
    @Synchronized
    public fun unregister() {
        UpdateEngineRegistry.removeFactory(factory)
    }

    private class Engine(
        private val dsg: DatasetGraphSparkles,
        inputBinding: Binding?,
        context: Context?,
    ) : UpdateEngine {
        private val context: Context = Context.setupContextForDataset(context, dsg)
        private var ownTxn = false
        private var failed = false
        private var pending = UpdateRequest()
        private val hasInput = inputBinding != null && !inputBinding.isEmpty
        private val worker by lazy {
            val constructors = UpdateEngineWorker::class.java.constructors
            val legacy = constructors.firstOrNull { it.parameterCount == 3 }
            (if (legacy != null) legacy.newInstance(dsg, inputBinding, this.context)
            else constructors.first { it.parameterCount == 2 }.newInstance(dsg, this.context)) as UpdateEngineWorker
        }
        private val sink = SinkImpl()

        override fun startRequest() {
            val t = dsg.txn()
            if (t == null) {
                dsg.begin(TxnType.WRITE)
                ownTxn = true
            } else {
                dsg.writeTxnForRequest()
            }
        }

        override fun finishRequest() {
            try {
                if (!failed) flushPending()
            } catch (e: RuntimeException) {
                failed = true
                throw e
            } finally {
                if (ownTxn) {
                    ownTxn = false
                    if (failed) {
                        try {
                            dsg.abort()
                        } finally {
                            dsg.end()
                        }
                    } else {
                        dsg.commit()
                        dsg.end()
                    }
                }
            }
        }

        override fun getUpdateSink(): UpdateSink = sink

        private inline fun guarded(block: () -> Unit) {
            try {
                block()
            } catch (e: RuntimeException) {
                failed = true
                throw e
            } catch (e: Error) {
                failed = true
                throw e
            }
        }

        /** Send the pending operations to Sparkles as one request. */
        fun flushPending() {
            if (pending.operations.isEmpty()) return
            val ops = pending.operations.toList()
            val text = pending.toString()
            pending = UpdateRequest()
            try {
                dsg.nativeUpdate(text, requestOptions(dsg, context, null))
                dsg.handle.nativeUpdates.addAndGet(ops.size.toLong())
            } catch (e: FfiException.Engine) {
                if (e.kind == ErrorKind.SPARQL_SYNTAX && fallbackMode(dsg, context) != SparklesFallback.NEVER) {
                    // a syntax error changed nothing: run the operations in ARQ
                    log.debug("update runs in ARQ after Sparkles refused it: {}", e.detail)
                    ops.forEach(::runInArq)
                    return
                }
                if (e.kind == ErrorKind.SPARQL_SYNTAX) {
                    throw QueryExecException("Sparkles cannot run the update, and the fallback mode is NEVER: ${e.detail}")
                }
                throw mapError(e)
            } catch (e: InternalException) {
                throw SparklesInternalException("Internal", e.message ?: "a failure in the native library")
            }
        }

        fun runInArq(update: Update) {
            dsg.handle.fallbackUpdates.incrementAndGet()
            update.visit(worker)
        }

        private inner class SinkImpl : UpdateSink {
            override fun send(update: Update) = guarded {
                when (update) {
                    is UpdateDataInsert -> {
                        flushPending()
                        for (q in update.quads) dsg.add(q)
                        dsg.handle.nativeUpdates.incrementAndGet()
                    }
                    is UpdateDataDelete -> {
                        flushPending()
                        for (q in update.quads) dsg.delete(q)
                        dsg.handle.nativeUpdates.incrementAndGet()
                    }
                    else -> {
                        val reason = if (hasInput) {
                            "the request has an initial binding"
                        } else {
                            FallbackDetector.check(update, context, knownOf(dsg))
                        }
                        if (reason == null) {
                            pending.add(update)
                        } else {
                            if (fallbackMode(dsg, context) == SparklesFallback.NEVER) {
                                throw QueryExecException("the update needs ARQ, and the fallback mode is NEVER: $reason")
                            }
                            log.debug("update operation runs in ARQ: {}", reason)
                            flushPending()
                            runInArq(update)
                        }
                    }
                }
            }

            private fun quads(action: (Quad) -> Unit): Sink<Quad> = object : Sink<Quad> {
                override fun send(item: Quad) = guarded {
                    flushPending()
                    action(item)
                }

                override fun flush() {}
                override fun close() {}
            }

            override fun createInsertDataSink(): QuadDataAccSink = QuadDataAccSink(quads { dsg.add(it) })

            override fun createDeleteDataSink(): QuadDataAccSink = QuadDataAccSink(quads { dsg.delete(it) })

            override fun flush() = guarded { flushPending() }

            override fun close() = guarded { flushPending() }
        }
    }
}
